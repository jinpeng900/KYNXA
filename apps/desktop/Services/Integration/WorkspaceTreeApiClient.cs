using System.Net.Http.Json;
using System.Text.Json;

namespace KYNXA_Desktop.Services;

/// <summary>One mirrored entry of a conversation work directory.
/// 会话工作目录镜像中的一个条目。</summary>
public sealed record WorkspaceTreeEntry(
    string Path, string Name, string Kind, long SizeBytes, long ModifiedAt, string? Status);

/// <summary>A saved upload, surfaced through the read-only virtual attachment folder.
/// 通过只读虚拟附件目录呈现的已保存上传。</summary>
public sealed record WorkspaceTreeAttachment(
    Guid Id, Guid MessageId, string Name, long SizeBytes, string? ContentUrl, string? CreatedAt);

/// <summary>A work directory snapshot; the version lets callers skip redrawing an unchanged tree.
/// 工作目录快照；版本号让调用方跳过未变化的重绘。</summary>
public sealed record WorkspaceTreeSnapshot(
    string Version, bool Changed, string WorkspaceKind, string Root, bool Truncated,
    string? VirtualAttachmentFolder,
    IReadOnlyList<WorkspaceTreeEntry> Entries, IReadOnlyList<WorkspaceTreeAttachment> Attachments);

/// <summary>
/// Reads the read-only work directory mirror; file bytes still come from the preview and attachment endpoints.
/// 读取只读的工作目录镜像；文件字节仍由预览与附件接口提供，客户端不直接访问磁盘。
/// </summary>
public sealed class WorkspaceTreeApiClient : IDisposable
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private readonly HttpClient _http;
    private readonly Func<CancellationToken, Task> _ensureReady;
    private readonly bool _ownsHttp;

    public WorkspaceTreeApiClient() : this(new HttpClient
    {
        BaseAddress = ModelGatewayService.Address, Timeout = Timeout.InfiniteTimeSpan
    }, ModelGatewayService.EnsureReadyAsync, true) { }

    public WorkspaceTreeApiClient(HttpClient http, Func<CancellationToken, Task>? ensureReady = null, bool ownsHttp = false)
    {
        _http = http; _ensureReady = ensureReady ?? (_ => Task.CompletedTask); _ownsHttp = ownsHttp;
    }

    /// <summary>A null or unchanged <paramref name="since"/> version requests the full snapshot.
    /// 未提供或已变化的版本号会请求完整快照。</summary>
    public async Task<WorkspaceTreeSnapshot> ReadAsync(Guid conversationId, string? since = null,
        CancellationToken cancellationToken = default)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(30));
        await _ensureReady(deadline.Token);
        string path = $"/api/agent/workspace/tree?conversationId={conversationId:D}";
        if (!string.IsNullOrEmpty(since)) path += $"&since={Uri.EscapeDataString(since)}";
        using var response = await _http.GetAsync(path, deadline.Token);
        return await GatewayResponseReader.ReadAsync<WorkspaceTreeSnapshot>(response,
            UiText.Get("工作区文件树返回为空。"), UiText.Get("工作区文件树读取失败（HTTP {0}）。"), deadline.Token, JsonOptions);
    }

    public void Dispose() { if (_ownsHttp) _http.Dispose(); }
}
