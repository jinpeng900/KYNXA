using System.Diagnostics;
using System.Net.Http.Json;
using System.Text.Json;

namespace KYNXA_Desktop.Services;

/// <summary>Starts the bundled gateway on demand without stopping shared services.</summary>
public static class ModelGatewayService
{
    private static readonly SemaphoreSlim StartupLock = new(1, 1);
    private static readonly HttpClient Probe = new() { Timeout = TimeSpan.FromSeconds(1) };
    public static Uri Address => new(Environment.GetEnvironmentVariable("KYNXA_MODEL_API_URL")
        ?? "http://127.0.0.1:5218");

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
                throw new InvalidOperationException("缺少模型网关文件，请重新构建或安装 KYNXA。");
            // A shared gateway can outlive the desktop. Keep its current directory
            // outside the deployment layout so Visual Studio can replace AppX.
            var workingDirectory = Path.Combine(Environment.GetFolderPath(
                Environment.SpecialFolder.LocalApplicationData), "KYNXA", "Runtime");
            Directory.CreateDirectory(workingDirectory);
            var start = new ProcessStartInfo(FindNode())
            {
                UseShellExecute = false, CreateNoWindow = true,
                WorkingDirectory = workingDirectory
            };
            start.ArgumentList.Add(script);
            start.Environment["KYNXA_MODEL_API_PORT"] = address.Port.ToString();
            // Preserve the previous base for an explicitly configured relative path.
            start.Environment.TryGetValue("KYNXA_MODEL_HOME", out var modelHome);
            if (!string.IsNullOrWhiteSpace(modelHome) && !Path.IsPathFullyQualified(modelHome))
                start.Environment["KYNXA_MODEL_HOME"] = Path.GetFullPath(modelHome, Path.GetDirectoryName(script)!);
            using var process = Process.Start(start)
                ?? throw new InvalidOperationException("无法启动模型网关。");
            var deadline = Stopwatch.StartNew();
            while (deadline.Elapsed < TimeSpan.FromSeconds(15))
            {
                cancellationToken.ThrowIfCancellationRequested();
                if (await IsReadyAsync(address, cancellationToken)) return;
                if (process.HasExited)
                    throw new InvalidOperationException("模型网关启动失败，请检查端口是否被占用及数据目录是否可访问。");
                await Task.Delay(200, cancellationToken);
            }
            throw new InvalidOperationException("模型网关启动超时，请稍后重试。");
        }
        catch (System.ComponentModel.Win32Exception error)
        {
            throw new InvalidOperationException("无法启动模型网关，请安装 Node.js 22.19 或更新版本后重新打开 KYNXA。", error);
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
            return body?.RootElement.TryGetProperty("service", out var service) == true
                && service.GetString() == "kynxa-model-gateway"
                && body.RootElement.TryGetProperty("status", out var status) && status.GetString() == "ok";
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
