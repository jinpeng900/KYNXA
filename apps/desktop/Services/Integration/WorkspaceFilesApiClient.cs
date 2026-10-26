using System.Text.Json;

namespace KYNXA_Desktop.Services;

public sealed record WorkspaceFileEntry(string Name, string Type);
public sealed record WorkspaceDirectoryListing(string Path, WorkspaceFileEntry[] Entries, bool Truncated = false);

/// <summary>
/// Rust resolves and authorizes each mount; the desktop only projects its directory receipt.
/// 挂载根解析与授权归 Rust；桌面仅展示目录回执，不自行扩大文件访问范围。
/// </summary>
public sealed class WorkspaceFilesApiClient : IDisposable
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private readonly HttpClient _http = GatewayHttpClient.Create(ModelGatewayService.Address, TimeSpan.FromSeconds(20));

    public async Task<WorkspaceDirectoryListing> ListAsync(Guid conversationId, Guid? mountId, string? path,
        CancellationToken cancellationToken = default)
    {
        await ModelGatewayService.EnsureReadyAsync(cancellationToken);
        string query = $"/api/agent/workspace/files?conversationId={conversationId:D}";
        if (mountId is { } id) query += $"&mountId={id:D}";
        if (!string.IsNullOrWhiteSpace(path)) query += "&path=" + Uri.EscapeDataString(path);
        using var response = await _http.GetAsync(query, cancellationToken);
        return await GatewayResponseReader.ReadAsync<WorkspaceDirectoryListing>(response,
            UiText.Get("文件列表返回为空。"), UiText.Get("文件列表读取失败（HTTP {0}）。"), cancellationToken, JsonOptions);
    }

    public void Dispose() => _http.Dispose();
}
