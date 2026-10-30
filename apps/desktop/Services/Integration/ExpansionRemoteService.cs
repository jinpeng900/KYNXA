using System.Diagnostics;
using System.Net.Http.Headers;
using System.Net.Http.Json;
using System.Security.Cryptography;
using System.Text.Json;

namespace KYNXA_Desktop.Services;

/// <summary>Owns only the expansion's optional LAN companion process; browser code never receives its admin token.</summary>
internal static class ExpansionRemoteService
{
    private static readonly SemaphoreSlim Lifecycle = new(1, 1);
    private static readonly HashSet<string> Actions = new(StringComparer.Ordinal)
        { "remote.status", "remote.approve", "remote.reject", "remote.revoke", "remote.grantConversation", "remote.registerDownload" };
    private static Process? _process;
    private static string? _adminToken, _tokenFile;
    private static int _port = 55220;
    public static Func<(string? ProviderId, string? ModelId)>? ModelSelection { get; set; }

    public static async Task StartAsync(Guid conversationId, CancellationToken cancellationToken = default)
    {
        await Lifecycle.WaitAsync(cancellationToken);
        try
        {
            if (_process is { HasExited: false }) return;
            StopOwnedProcess();
            await ModelGatewayService.EnsureReadyAsync(cancellationToken);
            string node = Path.Combine(AppContext.BaseDirectory, "runtime", "mcp", "node", "node.exe");
            string script = Path.Combine(AppContext.BaseDirectory, "tools", "workbench-remote.mjs");
            if (!File.Exists(node) || !File.Exists(script)) throw new InvalidOperationException("缺少随包手机连接运行时，请重新构建或安装 KYNXA。");
            string directory = Path.Combine(ApplicationPaths.DataRoot, "node-service");
            Directory.CreateDirectory(directory);
            _adminToken = Convert.ToHexString(RandomNumberGenerator.GetBytes(32)).ToLowerInvariant();
            _tokenFile = Path.Combine(directory, "admin-" + Guid.NewGuid().ToString("N") + ".token");
            await File.WriteAllTextAsync(_tokenFile, _adminToken, cancellationToken);
            if (int.TryParse(Environment.GetEnvironmentVariable("KYNXA_EXPANSION_REMOTE_PORT"), out int configured)
                && configured >= 1024 && configured <= 65535 && configured is not (5217 or 5218) && configured != ModelGatewayService.Address.Port)
                _port = configured;
            var start = new ProcessStartInfo(node)
            {
                UseShellExecute = false, CreateNoWindow = true, WorkingDirectory = AppContext.BaseDirectory,
                RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true
            };
            foreach (string argument in new[] { script, "--desktop-lifetime", "--host", "0.0.0.0", "--port", _port.ToString(), "--gateway-url",
                ModelGatewayService.Address.AbsoluteUri, "--admin-token-file", _tokenFile, "--data-dir", directory }) start.ArgumentList.Add(argument);
            _process = Process.Start(start) ?? throw new InvalidOperationException("无法启动手机连接服务。");
            // Drain private process pipes without placing pairing credentials or admin material in UI logs.
            _ = _process.StandardOutput.ReadToEndAsync(); _ = _process.StandardError.ReadToEndAsync();
            using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
            deadline.CancelAfter(TimeSpan.FromSeconds(8));
            while (true)
            {
                if (_process.HasExited) throw new InvalidOperationException("手机连接服务未能启动，请检查独立端口是否已被占用。");
                try { await SendAsync(conversationId, "remote.status", JsonSerializer.SerializeToElement(new { }), deadline.Token); return; }
                catch (HttpRequestException) { await Task.Delay(150, deadline.Token); }
            }
        }
        catch
        {
            StopOwnedProcess();
            throw;
        }
        finally { Lifecycle.Release(); }
    }

    public static async Task<JsonElement> RequestAsync(Guid conversationId, string action, JsonElement payload, CancellationToken cancellationToken)
    {
        if (!Actions.Contains(action)) throw new InvalidDataException("手机连接操作不可用。");
        if (_process is not { HasExited: false })
        {
            if (action == "remote.status") return JsonSerializer.SerializeToElement(new { online = false, pending = Array.Empty<object>(),
                devices = Array.Empty<object>(), notice = "手机连接尚未启动。开启后仅在当前局域网可连接。" });
            throw new InvalidOperationException("请先启动手机连接服务。");
        }
        string? id = payload.TryGetProperty("id", out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() : null;
        var normalized = new Dictionary<string, JsonElement>();
        foreach (var property in payload.EnumerateObject())
            if (property.Name is "requestId" or "deviceId" or "scopes"
                || action == "remote.registerDownload" && property.Name is "path" or "version" or "title" or "contentUrl")
                normalized[property.Name] = property.Value.Clone();
        if (!string.IsNullOrEmpty(id)) normalized[action is "remote.revoke" or "remote.grantConversation" ? "deviceId" : "requestId"] = JsonSerializer.SerializeToElement(id);
        return await SendAsync(conversationId, action, JsonSerializer.SerializeToElement(normalized), cancellationToken);
    }

    private static async Task<JsonElement> SendAsync(Guid conversationId, string action, JsonElement payload, CancellationToken cancellationToken)
    {
        var body = payload.EnumerateObject().ToDictionary(property => property.Name, property => property.Value.Clone(), StringComparer.Ordinal);
        if (ModelSelection?.Invoke() is { } model)
        {
            if (!string.IsNullOrEmpty(model.ProviderId)) body["providerId"] = JsonSerializer.SerializeToElement(model.ProviderId);
            if (!string.IsNullOrEmpty(model.ModelId)) body["modelId"] = JsonSerializer.SerializeToElement(model.ModelId);
        }
        using var http = GatewayHttpClient.Create(new Uri("http://127.0.0.1:" + _port), TimeSpan.FromSeconds(8));
        http.DefaultRequestHeaders.Authorization = new AuthenticationHeaderValue("Bearer", _adminToken);
        using var response = await http.PostAsJsonAsync("/admin/workbench", new { action, conversationId, payload = body }, cancellationToken);
        if (response.Content.Headers.ContentLength > 2 * 1024 * 1024) throw new InvalidDataException("手机连接状态过大。");
        using var result = await GatewayResponseReader.ReadAsync<JsonDocument>(response,
            "手机连接状态为空。", "手机连接操作失败（HTTP {0}）。", cancellationToken);
        return result.RootElement.Clone();
    }

    public static async Task StopAsync()
    {
        await Lifecycle.WaitAsync();
        try { StopOwnedProcess(); }
        finally { Lifecycle.Release(); }
    }

    private static void StopOwnedProcess()
    {
        if (_process is { } process)
        {
            try { if (!process.HasExited) process.Kill(entireProcessTree: true); }
            catch (InvalidOperationException) { }
            process.Dispose(); _process = null;
        }
        if (_tokenFile is { } file)
        {
            try { File.Delete(file); }
            catch (IOException) { }
            _tokenFile = null;
        }
        _adminToken = null;
    }
}
