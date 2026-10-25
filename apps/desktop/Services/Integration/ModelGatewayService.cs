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
    private static readonly object ReadinessSync = new();
    private static readonly CancellationTokenSource ShutdownCancellation = new();
    private static bool _shutdownRequested;
    private static Process? _ownedGateway;
    private static Task? _readinessTask;
    private static Uri? _readinessAddress;
    private static readonly HttpClient Probe = new(new SocketsHttpHandler { UseProxy = false })
        { Timeout = TimeSpan.FromSeconds(1) };
    public static string? LegacyDesktopDirectory { get; set; }
    public static Uri Address => new(Environment.GetEnvironmentVariable("KYNXA_MODEL_API_URL")
        ?? "http://127.0.0.1:5218");

    /// <summary>Send the saved selection to the background preparation service after gateway readiness.</summary>
    /// <remarks>网关就绪后提交已存选择，后台准备权重，不阻塞消息发送。</remarks>
    public static async Task WarmUpModelsAsync(string? providerId = null, string? modelId = null,
        CancellationToken cancellationToken = default)
    {
        using var preparation = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken, ShutdownCancellation.Token);
        using var client = new ModelApiClient();
        await client.SchedulePreparationAsync(providerId, modelId, preparation.Token);
    }

    /// <summary>Await local shutdown acceptance; the gateway owns durable task suspension and cleanup.</summary>
    /// <remarks>仅等待本机接受退出；任务中断保存和资源清理由网关自行完成。</remarks>
    public static async Task RequestShutdownAsync(CancellationToken cancellationToken = default)
    {
        Process? ownedGateway;
        lock (ReadinessSync)
        {
            if (_shutdownRequested) return;
            _shutdownRequested = true;
            ownedGateway = _ownedGateway;
        }
        // Stop startup and warmup before notifying the gateway, so they cannot start a replacement during exit.
        // 先停止启动及预热，再通知网关退出，避免收尾期间又拉起新网关。
        ShutdownCancellation.Cancel();
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(2));
        if (ownedGateway is not null)
        {
            try
            {
                if (!ownedGateway.HasExited)
                {
                    // The owned pipe accepts shutdown before HTTP is listening; process exit also closes it as EOF.
                    // 自有管道在 HTTP 监听前即可接收退出；前台结束也会关闭写端并发送 EOF。
                    await ownedGateway.StandardInput.WriteLineAsync("shutdown").WaitAsync(deadline.Token).ConfigureAwait(false);
                    await ownedGateway.StandardInput.FlushAsync(deadline.Token).ConfigureAwait(false);
                    return;
                }
            }
            catch (Exception error) when (error is IOException or InvalidOperationException or OperationCanceledException)
            {
                Debug.WriteLine($"Owned gateway shutdown signal: {error.GetType().Name}");
            }
        }
        Uri address = Address;
        if (!address.IsLoopback || address.Scheme != "http") return;
        using var client = GatewayHttpClient.Create(address, TimeSpan.FromSeconds(2));
        using var response = await client.PostAsJsonAsync("/api/lifecycle/shutdown", new
        {
            modelDataHome = ApplicationPaths.ModelHome,
            extensionRoot = ExtensionPaths.Root
        }, deadline.Token).ConfigureAwait(false);
        response.EnsureSuccessStatusCode();
        using var body = await response.Content.ReadFromJsonAsync<JsonDocument>(deadline.Token).ConfigureAwait(false);
        if (response.StatusCode != System.Net.HttpStatusCode.Accepted || body is null
            || !body.RootElement.TryGetProperty("accepted", out var accepted) || accepted.ValueKind != JsonValueKind.True)
            throw new InvalidOperationException("Gateway shutdown was not accepted. / 网关未接受退出请求。");
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
            WorkingDirectory = ApplicationPaths.Root,
            RedirectStandardOutput = true, RedirectStandardError = true
        };
        start.ArgumentList.Add("--initialize-storage");
        start.ArgumentList.Add(Path.GetFullPath(target));
        start.Environment["KYNXA_APPLICATION_ROOT"] = ApplicationPaths.Root;
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

    public static Task EnsureReadyAsync(CancellationToken cancellationToken = default)
    {
        cancellationToken.ThrowIfCancellationRequested();
        ShutdownCancellation.Token.ThrowIfCancellationRequested();
        var address = Address;
        if (address.Scheme != "http" || address.Host is not ("127.0.0.1" or "localhost")) return Task.CompletedTask;
        Task readiness;
        lock (ReadinessSync)
        {
            if (_shutdownRequested) throw new OperationCanceledException(ShutdownCancellation.Token);
            ShutdownCancellation.Token.ThrowIfCancellationRequested();
            // Concurrent catalog, preparation and chat requests share one readiness check.
            // 目录、准备和聊天并发共用一次检查；后续请求仍核对协议及存储范围。
            if (_readinessTask is null || _readinessTask.IsCompleted || _readinessAddress != address)
            {
                _readinessAddress = address;
                _readinessTask = Task.Run(() => EnsureLocalReadyAsync(address));
            }
            readiness = _readinessTask;
        }
        // Cancelling one caller must not cancel startup required by the other callers.
        // 单个调用方取消等待，不取消其他调用方仍需要的共享启动。
        return readiness.WaitAsync(cancellationToken);
    }

    private static async Task EnsureLocalReadyAsync(Uri address)
    {
        using var cancellation = CancellationTokenSource.CreateLinkedTokenSource(ShutdownCancellation.Token);
        cancellation.CancelAfter(TimeSpan.FromSeconds(20));
        CancellationToken cancellationToken = cancellation.Token;
        await StartupLock.WaitAsync(cancellationToken);
        try
        {
            if (await IsReadyAsync(address, cancellationToken)) return;
            var start = new ProcessStartInfo(FindNativeGateway())
            {
                UseShellExecute = false, CreateNoWindow = true,
                WorkingDirectory = ApplicationPaths.Root,
                RedirectStandardInput = true
            };
            start.ArgumentList.Add("--desktop-lifetime");
            start.Environment["KYNXA_APPLICATION_ROOT"] = ApplicationPaths.Root;
            start.Environment["KYNXA_MODEL_API_PORT"] = address.Port.ToString();
            ConfigureNativeRuntime(start);
            if (!string.IsNullOrWhiteSpace(LegacyDesktopDirectory))
                start.Environment["KYNXA_LEGACY_DESKTOP_HOME"] = LegacyDesktopDirectory;
            Process started;
            lock (ReadinessSync)
            {
                if (_shutdownRequested) throw new OperationCanceledException(ShutdownCancellation.Token);
                cancellationToken.ThrowIfCancellationRequested();
                if (_ownedGateway is { HasExited: false } existing)
                {
                    started = existing;
                }
                else
                {
                    _ownedGateway?.Dispose();
                    started = Process.Start(start)
                        ?? throw new InvalidOperationException(UiText.Get("无法启动模型网关。"));
                    _ownedGateway = started;
                }
            }
            var process = started;
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
            while (true)
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
                if (ready && (!HasMatchingPath(body!.RootElement, "modelDataHome", ApplicationPaths.ModelHome)
                    || !HasMatchingPath(body.RootElement, "extensionRoot", ExtensionPaths.Root)))
                    throw new InvalidOperationException(UiText.Get("桌面与网关的数据目录不一致，请统一启动配置后再迁移。"));
                if (ready && body!.RootElement.TryGetProperty("shuttingDown", out var shuttingDown)
                    && shuttingDown.ValueKind == JsonValueKind.True)
                {
                    // A matching gateway retires its own tasks before replacement; normal healthy probes never wait.
                    // 同范围网关先完成自身退出再替换；正常就绪探测不增加等待。
                    await Task.Delay(100, cancellationToken);
                    continue;
                }
                return ready;
            }
        }
        catch (Exception error) when (error is HttpRequestException or JsonException or OperationCanceledException)
        {
            cancellationToken.ThrowIfCancellationRequested();
            return false;
        }
    }

    private static bool HasMatchingPath(JsonElement body, string property, string expected)
    {
        if (!body.TryGetProperty(property, out var value) || value.ValueKind != JsonValueKind.String
            || value.GetString() is not string path || !Path.IsPathFullyQualified(path)) return false;
        try
        {
            return string.Equals(Path.TrimEndingDirectorySeparator(Path.GetFullPath(path)),
                Path.TrimEndingDirectorySeparator(Path.GetFullPath(expected)), StringComparison.OrdinalIgnoreCase);
        }
        catch (ArgumentException) { return false; }
    }

    // Resolve bundled native dependencies without modifying the user's global PATH.
    // 只给本次子进程加入随包原生依赖目录，不修改用户全局 PATH。
    private static void ConfigureNativeRuntime(ProcessStartInfo start)
    {
        // Pass writable defaults and pointer locations without freezing future migration reloads.
        // 传递可写默认位置和指针；不把当前选择锁成显式环境覆盖，迁移后仍可重新解析。
        start.Environment["KYNXA_STORAGE_POINTER"] = ApplicationPaths.StoragePointerPath;
        start.Environment["KYNXA_STORAGE_MIGRATION_LOCK"] = ApplicationPaths.MigrationLockPath;
        start.Environment["KYNXA_DEFAULT_DATA_HOME"] = ApplicationPaths.DefaultDataRoot;
        start.Environment["KYNXA_DEFAULT_MODEL_HOME"] = ApplicationPaths.DefaultModelHome;
        bool isolatedData = new[] { "KYNXA_DATA_HOME", "KYNXA_MODEL_HOME" }
            .Any(name => !string.IsNullOrWhiteSpace(Environment.GetEnvironmentVariable(name)));
        bool explicitExtensionPointer = !string.IsNullOrWhiteSpace(Environment.GetEnvironmentVariable("KYNXA_EXTENSION_POINTER"));
        if (!isolatedData || explicitExtensionPointer)
            start.Environment["KYNXA_EXTENSION_POINTER"] = ExtensionPaths.PointerPath;
        start.Environment["KYNXA_DEFAULT_EXTENSION_HOME"] = ExtensionPaths.LegacyRoot ?? ExtensionPaths.DefaultRoot;
        if (ExtensionPaths.LegacyRoot is string legacyRoot && string.Equals(
            Path.TrimEndingDirectorySeparator(Path.GetFullPath(legacyRoot)), ApplicationPaths.DataRoot, StringComparison.OrdinalIgnoreCase))
            start.Environment["KYNXA_EXTENSIONS_FOLLOW_DATA"] = "1";
        else start.Environment.Remove("KYNXA_EXTENSIONS_FOLLOW_DATA");
        string nativeDirectory = Path.Combine(ApplicationPaths.Root, "runtime", "native");
        start.Environment["PATH"] = nativeDirectory + Path.PathSeparator
            + (start.Environment.TryGetValue("PATH", out string? currentPath) ? currentPath : "");
    }

    /// <summary>Launch the packaged native gateway; never search for or fall back to Node.</summary>
    /// <remarks>只启动随包原生网关；缺失时明确报错，不查找或回退 Node。</remarks>
    private static string FindNativeGateway()
    {
        var bundled = Path.Combine(ApplicationPaths.Root, "runtime", "agent", "kynxa-gateway.exe");
        if (File.Exists(bundled)) return bundled;
        throw new FileNotFoundException(UiText.Get("缺少模型网关文件，请重新构建或安装 KYNXA。"), bundled);
    }
}
