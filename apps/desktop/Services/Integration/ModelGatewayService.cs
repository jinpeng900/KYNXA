using System.Diagnostics;
using System.Net.Http.Json;
using System.Text.Json;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Starts the bundled gateway on demand without stopping shared services.
/// 按需启动随包网关，不停止其他调用方共用的服务。
/// </summary>
public static class ModelGatewayService
{
    private static readonly SemaphoreSlim StartupLock = new(1, 1);
    private static readonly HttpClient Probe = new() { Timeout = TimeSpan.FromSeconds(1) };
    public static string? LegacyDesktopDirectory { get; set; }
    public static Uri Address => new(Environment.GetEnvironmentVariable("KYNXA_MODEL_API_URL")
        ?? "http://127.0.0.1:5218");

    /// <summary>Send the saved selection to the background preparation service after gateway readiness.</summary>
    /// <remarks>网关就绪后只提交已存选择到后台准备服务；不等待权重就绪，不加入消息发送前置。</remarks>
    public static async Task WarmUpModelsAsync(string? providerId = null, string? modelId = null,
        CancellationToken cancellationToken = default)
    {
        await EnsureReadyAsync(cancellationToken);
        using var client = new ModelApiClient();
        await client.SchedulePreparationAsync(providerId, modelId, cancellationToken);
    }

    /// <summary>
    /// Validate and prepare a migration copy before its pointer becomes active.
    /// 启用迁移后的路径指针之前，验证并准备迁移副本。
    /// </summary>
    public static async Task InitializeStorageAsync(string target, CancellationToken cancellationToken = default)
    {
        cancellationToken.ThrowIfCancellationRequested();
        if (!Path.IsPathFullyQualified(target)) throw new InvalidOperationException(UiText.Get("数据目录必须使用绝对路径。"));
        string executable = FindNativeGateway();
        var start = new ProcessStartInfo(executable)
        {
            UseShellExecute = false, CreateNoWindow = true,
            WorkingDirectory = AppContext.BaseDirectory,
            RedirectStandardOutput = true, RedirectStandardError = true
        };
        start.ArgumentList.Add("--initialize-storage");
        start.ArgumentList.Add(Path.GetFullPath(target));
        start.Environment["KYNXA_APPLICATION_ROOT"] = AppContext.BaseDirectory;
        ConfigureNativeRuntime(start);
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
                // 此短期辅助进程只属于本次迁移；不得停止共享网关。
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
            throw new InvalidOperationException(UiText.Get("无法启动存储初始化程序，请检查安装文件与运行权限。"), error);
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
            var start = new ProcessStartInfo(FindNativeGateway())
            {
                UseShellExecute = false, CreateNoWindow = true,
                WorkingDirectory = AppContext.BaseDirectory
            };
            start.Environment["KYNXA_APPLICATION_ROOT"] = AppContext.BaseDirectory;
            start.Environment["KYNXA_MODEL_API_PORT"] = address.Port.ToString();
            ConfigureNativeRuntime(start);
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
            throw new InvalidOperationException(UiText.Get("无法启动模型网关，请检查安装文件与运行权限。"), error);
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
            if (ready && (!body!.RootElement.TryGetProperty("runtime", out var runtime) || runtime.GetString() != "rust"))
                throw new InvalidOperationException(UiText.Get("正在运行的旧网关不支持当前聊天、记忆与工具接口。请在当前回复结束后关闭旧网关，再重新打开 KYNXA。"));
            if (ready && (!body!.RootElement.TryGetProperty("conversationProtocol", out var protocol) ||
                !protocol.TryGetInt32(out int version) || version < 1 ||
                !body.RootElement.TryGetProperty("dataLayoutVersion", out var layout) ||
                !layout.TryGetInt32(out int layoutVersion) || layoutVersion < 1 ||
                !body.RootElement.TryGetProperty("memoryProtocol", out var memory) ||
                !memory.TryGetInt32(out int memoryVersion) || memoryVersion < 1 ||
                !body.RootElement.TryGetProperty("contextProtocol", out var context) ||
                !context.TryGetInt32(out int contextVersion) || contextVersion < 3 ||
                !body.RootElement.TryGetProperty("agentProtocol", out var agent) ||
                !agent.TryGetInt32(out int agentVersion) || agentVersion < 5 ||
                !body.RootElement.TryGetProperty("officialToolsProtocol", out var officialTools) ||
                !officialTools.TryGetInt32(out int officialToolsVersion) || officialToolsVersion < 2 ||
                !body.RootElement.TryGetProperty("hostTerminalProtocol", out var hostTerminal) ||
                !hostTerminal.TryGetInt32(out int hostTerminalVersion) || hostTerminalVersion < 3 ||
                !body.RootElement.TryGetProperty("browserAutomationProtocol", out var browserAutomation) ||
                !browserAutomation.TryGetInt32(out int browserAutomationVersion) || browserAutomationVersion < 2 ||
                !body.RootElement.TryGetProperty("extensionStorageProtocol", out var extensions) ||
                !extensions.TryGetInt32(out int extensionVersion) || extensionVersion < 1 ||
                !body.RootElement.TryGetProperty("toolStreamProtocol", out var toolStream) ||
                !toolStream.TryGetInt32(out int toolStreamVersion) || toolStreamVersion < 3 ||
                !body.RootElement.TryGetProperty("retrievalProtocol", out var retrieval) ||
                !retrieval.TryGetInt32(out int retrievalVersion) || retrievalVersion < 1))
                throw new InvalidOperationException(UiText.Get("正在运行的旧网关不支持当前聊天、记忆与工具接口。请在当前回复结束后关闭旧网关，再重新打开 KYNXA。"));
            return ready;
        }
        catch (Exception error) when (error is HttpRequestException or JsonException or OperationCanceledException)
        {
            cancellationToken.ThrowIfCancellationRequested();
            return false;
        }
    }

    // Resolve bundled native dependencies without modifying the user's global PATH.
    // 只给本次子进程加入随包原生依赖目录，不修改用户全局 PATH。
    private static void ConfigureNativeRuntime(ProcessStartInfo start)
    {
        string nativeDirectory = Path.Combine(AppContext.BaseDirectory, "runtime", "native");
        start.Environment["PATH"] = nativeDirectory + Path.PathSeparator
            + (start.Environment.TryGetValue("PATH", out string? currentPath) ? currentPath : "");
    }

    /// <summary>Launch the packaged native gateway; never search for or fall back to Node.</summary>
    /// <remarks>只启动随包原生网关；缺失时明确报错，不查找或回退 Node。</remarks>
    private static string FindNativeGateway()
    {
        var bundled = Path.Combine(AppContext.BaseDirectory, "runtime", "agent", "kynxa-gateway.exe");
        if (File.Exists(bundled)) return bundled;
        throw new FileNotFoundException(UiText.Get("缺少模型网关文件，请重新构建或安装 KYNXA。"), bundled);
    }
}
