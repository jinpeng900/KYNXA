using System.Diagnostics;
using System.Net.Http.Json;
using System.Text.Json;
using KYNXA.Contracts;

/// <summary>
/// Runs only a declared native bundle with owned, synthetic data and no model calls.
/// 仅启动显式指定的原生包，使用自有虚构资料，禁用语义推理且不访问模型接口。
/// </summary>
internal sealed class NativeGatewayFixture : IAsyncDisposable
{
    private const string TemporaryPrefix = "kynxa-native-retrieval-live-";
    private readonly Process _process;
    private readonly Task<string> _errors;
    private readonly Task<string> _output;
    public string Root { get; }
    public Uri Address { get; }
    public Guid ProjectId { get; }
    public Guid OtherProjectId { get; }
    public Guid ConversationId { get; }
    public string GlobalSource { get; }
    public string ProjectSource { get; }
    public string OtherSource { get; }

    private NativeGatewayFixture(Process process, Task<string> errors, Task<string> output, string root,
        Uri address, Guid projectId, Guid otherProjectId, Guid conversationId)
    {
        _process = process;
        _errors = errors;
        _output = output;
        Root = root;
        Address = address;
        ProjectId = projectId;
        OtherProjectId = otherProjectId;
        ConversationId = conversationId;
        GlobalSource = Path.Combine(root, "sources", "global-source.txt");
        ProjectSource = Path.Combine(root, "sources", "project-source.txt");
        OtherSource = Path.Combine(root, "sources", "other-source.txt");
    }

    public static async Task<NativeGatewayFixture> StartAsync(string[] args)
    {
        if (!OperatingSystem.IsWindows()) throw new PlatformNotSupportedException("The native gateway bundle is Windows-only.");
        int bundleOption = Array.IndexOf(args, "--native-bundle");
        string? selected = bundleOption >= 0
            ? args.ElementAtOrDefault(bundleOption + 1)
            : Environment.GetEnvironmentVariable("KYNXA_NATIVE_BUNDLE");
        if (string.IsNullOrWhiteSpace(selected) || selected.StartsWith("--", StringComparison.Ordinal))
            throw new ArgumentException("--live requires --native-bundle <application directory> or KYNXA_NATIVE_BUNDLE; it never starts a user gateway.");
        string bundle = Path.GetFullPath(selected);
        string executable = Path.Combine(bundle, "runtime", "agent", "kynxa-gateway.exe");
        foreach (string relative in new[] { "runtime/agent/kynxa-gateway.exe", "runtime/resource/kynxa-resource-service.exe",
            "runtime/native/vec0.dll", "runtime/native/onnxruntime.dll", "ToolHost/KYNXA.ToolHost.exe" })
            if (!File.Exists(Path.Combine(bundle, relative.Replace('/', Path.DirectorySeparatorChar))))
                throw new FileNotFoundException($"Native bundle file missing: {relative}", bundle);

        string root = Path.Combine(Path.GetTempPath(), TemporaryPrefix + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(root);
        Process? process = null;
        try
        {
            Guid project = Guid.NewGuid(), otherProject = Guid.NewGuid(), chat = Guid.NewGuid();
            string data = Path.Combine(root, "Data"), sources = Path.Combine(root, "sources");
            foreach (string folder in new[] { data, sources, Path.Combine(root, "workspace"), Path.Combine(root, "other-workspace"),
                Path.Combine(root, "Legacy"), Path.Combine(root, "Extensions"), Path.Combine(data, "Models"), Path.Combine(data, "Retrieval") })
                Directory.CreateDirectory(folder);
            await File.WriteAllTextAsync(Path.Combine(sources, "global-source.txt"), "GLOBAL_RETRIEVAL_FIXTURE 中文资料：全局测试资料。");
            await File.WriteAllTextAsync(Path.Combine(sources, "project-source.txt"), "PROJECT_RETRIEVAL_FIXTURE 中文资料：工作测试资料。");
            await File.WriteAllTextAsync(Path.Combine(sources, "other-source.txt"), "OTHER_SCOPE_PRIVATE_FIXTURE 只属于另一个工作。");
            var catalog = new
            {
                Version = 1, Revision = 0, Tombstones = Array.Empty<object>(),
                Projects = new object[]
                {
                    new { Id = project, Name = "Synthetic retrieval work", FolderPath = Path.Combine(root, "workspace"), IsFolderlessWorkspace = false,
                        Chats = new[] { new { Id = chat, Title = "Synthetic saved chat", Messages = Array.Empty<object>() } } },
                    new { Id = otherProject, Name = "Separate scope", FolderPath = Path.Combine(root, "other-workspace"), IsFolderlessWorkspace = false,
                        Chats = Array.Empty<object>() }
                },
                Chats = Array.Empty<object>()
            };
            await File.WriteAllTextAsync(Path.Combine(data, "catalog.json"), JsonSerializer.Serialize(catalog));
            var settings = new RetrievalSettingsDocument(1, 0, new(true, "off", "builtin-multilingual"),
                new("auto", "auto", "standard", "auto"), new(64 * 1024 * 1024, 512 * 1024 * 1024));
            await File.WriteAllTextAsync(Path.Combine(data, "Retrieval", "settings.json"),
                JsonSerializer.Serialize(settings, new JsonSerializerOptions(JsonSerializerDefaults.Web)));

            var start = new ProcessStartInfo(executable)
            {
                WorkingDirectory = bundle, UseShellExecute = false, CreateNoWindow = true,
                RedirectStandardOutput = true, RedirectStandardError = true
            };
            start.Environment["KYNXA_APPLICATION_ROOT"] = bundle;
            start.Environment["KYNXA_MODEL_API_PORT"] = "0";
            start.Environment["KYNXA_MODEL_HOME"] = Path.Combine(data, "Models");
            start.Environment["KYNXA_DATA_HOME"] = data;
            start.Environment["KYNXA_EXTENSION_HOME"] = Path.Combine(root, "Extensions");
            start.Environment["KYNXA_LEGACY_DESKTOP_HOME"] = Path.Combine(root, "Legacy");
            start.Environment.Remove("KYNXA_EXTENSION_POINTER");
            start.Environment["PATH"] = Path.Combine(bundle, "runtime", "native") + Path.PathSeparator + start.Environment["PATH"];
            process = Process.Start(start) ?? throw new InvalidOperationException("Could not start the owned native fixture.");
            Task<string> errors = process.StandardError.ReadToEndAsync();
            int port = 0;
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
            while (port == 0)
            {
                string? line = await process.StandardOutput.ReadLineAsync(deadline.Token);
                if (line is null) throw new InvalidOperationException($"Native fixture exited before readiness: {await errors}");
                try
                {
                    using var ready = JsonDocument.Parse(line);
                    if (ready.RootElement.TryGetProperty("service", out var service) && service.GetString() == "kynxa-model-gateway")
                    {
                        if (ready.RootElement.GetProperty("runtime").GetString() != "rust" || ready.RootElement.GetProperty("nodeRuntime").GetBoolean())
                            throw new InvalidDataException("Fixture did not start the independent Rust gateway.");
                        port = ready.RootElement.GetProperty("port").GetInt32();
                    }
                }
                catch (JsonException) { /* Other startup diagnostics are not readiness. / 其他启动诊断不算就绪。 */ }
            }
            var address = new Uri($"http://127.0.0.1:{port}");
            Task<string> output = process.StandardOutput.ReadToEndAsync();
            using var http = new HttpClient { BaseAddress = address, Timeout = TimeSpan.FromSeconds(10) };
            using var health = await http.GetFromJsonAsync<JsonDocument>("/health");
            if (health?.RootElement.GetProperty("runtime").GetString() != "rust") throw new InvalidDataException("Native health is invalid.");
            return new(process, errors, output, root, address, project, otherProject, chat);
        }
        catch
        {
            if (process is not null)
            {
                if (!process.HasExited) { process.Kill(entireProcessTree: true); await process.WaitForExitAsync(); }
                process.Dispose();
            }
            DeleteOwnedRoot(root);
            throw;
        }
    }

    public async ValueTask DisposeAsync()
    {
        // The executable has no test shutdown endpoint; terminate only this owned process tree after completed checks.
        // 正式程序没有测试关闭接口；检查结束后仅终止夹具拥有的进程树，不触碰日常网关。
        if (!_process.HasExited) { _process.Kill(entireProcessTree: true); await _process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(15)); }
        string errors = await _errors;
        await _output;
        _process.Dispose();
        if (!string.IsNullOrWhiteSpace(errors)) Console.Error.WriteLine($"Owned native fixture diagnostics: {errors}");
        DeleteOwnedRoot(Root);
    }

    private static void DeleteOwnedRoot(string root)
    {
        string parent = Path.GetFullPath(Path.GetTempPath()).TrimEnd(Path.DirectorySeparatorChar);
        string resolved = Path.GetFullPath(root);
        if (Path.GetDirectoryName(resolved) != parent || !Path.GetFileName(resolved).StartsWith(TemporaryPrefix, StringComparison.Ordinal))
            throw new InvalidOperationException("Refusing cleanup outside the owned fixture directory.");
        Directory.Delete(resolved, recursive: true);
    }
}
