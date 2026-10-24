using System.Net.Http.Json;
using System.Text.Json;

namespace KYNXA_Desktop.Services;

public sealed record LocalModelInfo
{
    public string Id { get; init; } = "";
    public string Name { get; init; } = "";
    public string Family { get; init; } = "";
    public string Category { get; init; } = "";
    public JsonElement Parameters { get; init; }
    public string Quantization { get; init; } = "";
    public long DownloadBytes { get; init; }
    public double? RecommendedRamGb { get; init; }
    public double? RecommendedVramGb { get; init; }
    public string[] Capabilities { get; init; } = [];
    public bool Recommended { get; init; }
    public bool Installed { get; init; }
    public bool Enabled { get; init; }
    public string Status { get; init; } = "";
    public string? Path { get; init; }
    public long DownloadedBytes { get; init; }
    public JsonElement Error { get; init; }
    public string? SourceUrl { get; init; }
    public LocalModelRuntimeInfo? Runtime { get; init; }
}

public sealed record LocalModelRuntimeInfo(string State = "stopped", int ActiveRequests = 0, int? ProcessId = null,
    string? Backend = null, bool ModelIdentityVerified = false, string? LoadedModelPath = null,
    string? BaseUrl = null, bool? SupportsTools = null, string ToolCallingMode = "unknown",
    string ToolCapabilityState = "unknown", string? Code = null);

public sealed record LocalModelHardwareInfo
{
    public double? MemoryGb { get; init; }
    public double? AvailableMemoryGb { get; init; }
    public double? VramGb { get; init; }
    public string? GpuName { get; init; }
}

public sealed record LocalModelsSnapshot
{
    public bool Enabled { get; init; }
    public string InstallDirectory { get; init; } = "";
    public string LoadPolicy { get; init; } = "auto";
    public LocalModelInfo[] Models { get; init; } = [];
    public LocalModelHardwareInfo? Hardware { get; init; }
}

/// <summary>
/// The Rust owner installs and runs local models; this client only reads status and submits explicit actions.
/// Rust 拥有本地模型安装与运行；客户端只读取状态、提交用户明确操作，不直接管理模型文件。
/// </summary>
public sealed class LocalModelsApiClient : IDisposable
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private readonly HttpClient _http = GatewayHttpClient.Create(ModelGatewayService.Address, Timeout.InfiniteTimeSpan);

    public async Task<LocalModelsSnapshot> ReadAsync(CancellationToken token = default)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(token);
        deadline.CancelAfter(TimeSpan.FromSeconds(20));
        await ModelGatewayService.EnsureReadyAsync(deadline.Token).ConfigureAwait(false);
        using var response = await _http.GetAsync("/api/local-models", deadline.Token).ConfigureAwait(false);
        return await ReadSnapshotAsync(response, deadline.Token).ConfigureAwait(false);
    }

    public Task<LocalModelsSnapshot> SettingsAsync(bool? enabled = null, string? installDirectory = null,
        bool? moveExisting = null, string? loadPolicy = null, CancellationToken token = default) =>
        PostAsync("settings", new { enabled, installDirectory, moveExisting, loadPolicy }, token, allowLongOperation: installDirectory is not null);

    public Task<LocalModelsSnapshot> InstallAsync(string id, CancellationToken token = default) =>
        PostAsync("install", new { id }, token);

    public Task<LocalModelsSnapshot> ImportAsync(string path, string? name = null, bool copyToLibrary = false,
        string? mmprojPath = null, CancellationToken token = default) =>
        PostAsync("import", new { path, name, copyToLibrary, mmprojPath }, token, allowLongOperation: true);

    public Task<LocalModelsSnapshot> EnableAsync(string id, bool enabled, CancellationToken token = default) =>
        PostAsync("enable", new { id, enabled }, token);

    public Task<LocalModelsSnapshot> DeleteAsync(string id, CancellationToken token = default) =>
        PostAsync("delete", new { id }, token);

    public Task<LocalModelsSnapshot> DownloadAsync(string id, string action, CancellationToken token = default) =>
        PostAsync("download", new { id, action }, token);

    private async Task<LocalModelsSnapshot> PostAsync(string action, object body, CancellationToken token, bool allowLongOperation = false)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(token);
        // Moving/importing weights may take longer than an ordinary control request; the window only cancels its wait.
        // 权重迁移及导入可长于普通控制请求；窗口取消只结束等待，不撤回后端已接受的文件事务。
        if (!allowLongOperation) deadline.CancelAfter(TimeSpan.FromSeconds(45));
        await ModelGatewayService.EnsureReadyAsync(deadline.Token).ConfigureAwait(false);
        using var response = await _http.PostAsJsonAsync("/api/local-models/" + action, body, JsonOptions, deadline.Token).ConfigureAwait(false);
        return await ReadSnapshotAsync(response, deadline.Token).ConfigureAwait(false);
    }

    private static Task<LocalModelsSnapshot> ReadSnapshotAsync(HttpResponseMessage response, CancellationToken token) =>
        GatewayResponseReader.ReadAsync<LocalModelsSnapshot>(response, UiText.Get("本地模型管理返回为空。"),
            UiText.Get("本地模型操作失败（HTTP {0}）。"), token, JsonOptions);

    public void Dispose() => _http.Dispose();
}
