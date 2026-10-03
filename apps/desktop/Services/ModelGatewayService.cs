using System.Diagnostics;
using System.Net.Http.Json;
using System.Text.Json;

namespace KYNXA_Desktop.Services;

/// <summary>Starts the bundled gateway on demand without stopping shared services.</summary>
public static class ModelGatewayService
{
    private static readonly SemaphoreSlim StartupLock = new(1, 1);
    private static readonly HttpClient Probe = new() { Timeout = TimeSpan.FromSeconds(1) };
    public static string? LegacyDesktopDirectory { get; set; }
    public static Uri Address => new(Environment.GetEnvironmentVariable("KYNXA_MODEL_API_URL")
        ?? "http://127.0.0.1:5218");

    /// <summary>Validate and prepare a migration copy before its pointer becomes active.</summary>
    public static async Task InitializeStorageAsync(string target, CancellationToken cancellationToken = default)
    {
        cancellationToken.ThrowIfCancellationRequested();
        if (!Path.IsPathFullyQualified(target)) throw new InvalidOperationException(UiText.Get("数据目录必须使用绝对路径。"));
        string script = Path.Combine(AppContext.BaseDirectory, "model-gateway", "initialize-storage.mjs");
        if (!File.Exists(script)) throw new InvalidOperationException(UiText.Get("缺少存储初始化文件，请重新构建或安装 KYNXA。"));
        var start = new ProcessStartInfo(FindNode())
        {
            UseShellExecute = false, CreateNoWindow = true,
            WorkingDirectory = Path.GetDirectoryName(script)!,
            RedirectStandardOutput = true, RedirectStandardError = true
        };
        start.ArgumentList.Add(script);
        start.ArgumentList.Add(Path.GetFullPath(target));
        try
        {
            using var process = Process.Start(start) ?? throw new InvalidOperationException(UiText.Get("无法启动存储初始化程序。"));
            using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
            deadline.CancelAfter(TimeSpan.FromMinutes(2));
            Task<string> output = process.StandardOutput.ReadToEndAsync(), error = process.StandardError.ReadToEndAsync();
            try
            {
                await process.WaitForExitAsync(deadline.Token);
                await Task.WhenAll(output, error).WaitAsync(deadline.Token);
            }
            catch (OperationCanceledException)
            {
                // This short-lived helper belongs to this migration. Never stop the shared gateway.
                try { if (!process.HasExited) process.Kill(entireProcessTree: true); }
                catch (InvalidOperationException) { }
                await process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5));
                await Task.WhenAll(output, error).WaitAsync(TimeSpan.FromSeconds(5));
                cancellationToken.ThrowIfCancellationRequested();
                throw new InvalidOperationException(UiText.Get("存储初始化超时，原存储位置未改变。"));
            }
            if (process.ExitCode != 0)
            {
                string detail = error.Result.Trim();
                if (detail.Length > 1500) detail = detail[..1500];
                throw new InvalidOperationException(UiText.Get("存储初始化失败，原存储位置未改变。") + (detail.Length == 0 ? "" : "\n" + detail));
            }
        }
        catch (System.ComponentModel.Win32Exception error)
        {
            throw new InvalidOperationException(UiText.Get("无法启动存储初始化程序，请安装 Node.js 22.19 或更新版本。"), error);
        }
    }

    public static async Task EnsureReadyAsync(CancellationToken cancellationToken = default)
    {
        var address = Address;
        if (address.Scheme != "http" || address.Host is not ("127.0.0.1" or "localhost")) return;
        await StartupLock.WaitAsync(cancellationToken);
        try
        {
            if (await IsReadyAsync(address, cancellationToken)) return;
            var script = Path.Combine(AppContext.BaseDirectory, "model-gateway", "server.mjs");
            if (!File.Exists(script))
                throw new InvalidOperationException(UiText.Get("缺少模型网关文件，请重新构建或安装 KYNXA。"));
            var start = new ProcessStartInfo(FindNode())
            {
                UseShellExecute = false, CreateNoWindow = true,
                WorkingDirectory = Path.GetDirectoryName(script)!
            };
            start.ArgumentList.Add(script);
            start.Environment["KYNXA_MODEL_API_PORT"] = address.Port.ToString();
            if (!string.IsNullOrWhiteSpace(LegacyDesktopDirectory))
                start.Environment["KYNXA_LEGACY_DESKTOP_HOME"] = LegacyDesktopDirectory;
            using var process = Process.Start(start)
                ?? throw new InvalidOperationException(UiText.Get("无法启动模型网关。"));
            var deadline = Stopwatch.StartNew();
            while (deadline.Elapsed < TimeSpan.FromSeconds(15))
            {
                cancellationToken.ThrowIfCancellationRequested();
                if (await IsReadyAsync(address, cancellationToken)) return;
                if (process.HasExited)
                    throw new InvalidOperationException(UiText.Get("模型网关启动失败，请检查端口是否被占用及数据目录是否可访问。"));
                await Task.Delay(200, cancellationToken);
            }
            throw new InvalidOperationException(UiText.Get("模型网关启动超时，请稍后重试。"));
        }
        catch (System.ComponentModel.Win32Exception error)
        {
            throw new InvalidOperationException(UiText.Get("无法启动模型网关，请安装 Node.js 22.19 或更新版本后重新打开 KYNXA。"), error);
        }
        finally { StartupLock.Release(); }
    }

    private static async Task<bool> IsReadyAsync(Uri address, CancellationToken cancellationToken)
    {
        try
        {
            using var response = await Probe.GetAsync(new Uri(address, "/health"), cancellationToken);
            if (!response.IsSuccessStatusCode) return false;
            using var body = await response.Content.ReadFromJsonAsync<JsonDocument>(cancellationToken);
            bool ready = body?.RootElement.TryGetProperty("service", out var service) == true
                && service.GetString() == "kynxa-model-gateway"
                && body.RootElement.TryGetProperty("status", out var status) && status.GetString() == "ok";
            if (ready && (!body!.RootElement.TryGetProperty("conversationProtocol", out var protocol) ||
                !protocol.TryGetInt32(out int version) || version < 1 ||
                !body.RootElement.TryGetProperty("dataLayoutVersion", out var layout) ||
                !layout.TryGetInt32(out int layoutVersion) || layoutVersion < 1 ||
                !body.RootElement.TryGetProperty("memoryProtocol", out var memory) ||
                !memory.TryGetInt32(out int memoryVersion) || memoryVersion < 1 ||
                !body.RootElement.TryGetProperty("contextProtocol", out var context) ||
                !context.TryGetInt32(out int contextVersion) || contextVersion < 1 ||
                !body.RootElement.TryGetProperty("agentProtocol", out var agent) ||
                !agent.TryGetInt32(out int agentVersion) || agentVersion < 1 ||
                !body.RootElement.TryGetProperty("toolStreamProtocol", out var toolStream) ||
                !toolStream.TryGetInt32(out int toolStreamVersion) || toolStreamVersion < 1))
                throw new InvalidOperationException(UiText.Get("正在运行的旧网关不支持当前聊天记忆与上下文结构。请在当前回复结束后关闭旧网关，再重新打开 KYNXA。"));
            return ready;
        }
        catch (Exception error) when (error is HttpRequestException or JsonException or OperationCanceledException)
        {
            cancellationToken.ThrowIfCancellationRequested();
            return false;
        }
    }

    private static string FindNode()
    {
        var bundled = Path.Combine(AppContext.BaseDirectory, "runtime", "node.exe");
        if (File.Exists(bundled)) return bundled;
        foreach (var directory in (Environment.GetEnvironmentVariable("PATH") ?? "").Split(Path.PathSeparator))
        {
            if (string.IsNullOrWhiteSpace(directory)) continue;
            var candidate = Path.Combine(directory.Trim('"'), "node.exe");
            if (File.Exists(candidate)) return candidate;
        }
        var installed = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles), "nodejs", "node.exe");
        return File.Exists(installed) ? installed : "node.exe";
    }
}
