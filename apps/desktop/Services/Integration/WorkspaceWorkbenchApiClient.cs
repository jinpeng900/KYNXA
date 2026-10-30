using System.Net.Http.Json;
using System.Text.Json;

namespace KYNXA_Desktop.Services;

/// <summary>The trusted desktop fixes the conversation and mount for every workbench request.</summary>
public sealed class WorkspaceWorkbenchApiClient : IDisposable
{
    private static readonly HashSet<string> Actions = new(StringComparer.Ordinal)
    {
        "files.list", "files.read", "files.write", "versions.list", "versions.create", "versions.compare", "versions.restore",
        "deliverables.list", "search.conversations", "diagnostics", "templates.list", "templates.save", "templates.delete",
        "bookmarks.list", "bookmarks.save", "bookmarks.delete", "media.transcribe", "media.cancel", "project.read", "project.save"
    };
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private readonly HttpClient _http = GatewayHttpClient.Create(ModelGatewayService.Address, Timeout.InfiniteTimeSpan);

    public async Task<JsonElement> RequestAsync(Guid conversationId, Guid? mountId, string action, JsonElement payload,
        CancellationToken cancellationToken)
    {
        if (conversationId == Guid.Empty || !Actions.Contains(action) || payload.ValueKind != JsonValueKind.Object)
        {
            if (conversationId != Guid.Empty && payload.ValueKind == JsonValueKind.Object && action == "remote.registerDownload")
            {
                if (!payload.TryGetProperty("path", out var pathValue) || pathValue.ValueKind != JsonValueKind.String
                    || pathValue.GetString() is not { Length: > 0 and <= 4096 } path) throw new InvalidDataException("请选择要授权下载的文件。");
                using var previews = new WorkspacePreviewApiClient();
                var preview = await previews.ReadAsync(conversationId, path, userSelected: true, mountId: mountId, cancellationToken: cancellationToken);
                string? downloadUrl = preview.DownloadUrl ?? preview.ContentUrl;
                if (string.IsNullOrWhiteSpace(downloadUrl)) throw new InvalidOperationException("此文件没有可供下载的原件预览地址。");
                _ = previews.ContentAddress(downloadUrl);
                return await ExpansionRemoteService.RequestAsync(conversationId, action, JsonSerializer.SerializeToElement(new
                    { path = preview.Path, version = preview.Version, title = preview.Title, contentUrl = downloadUrl }), cancellationToken);
            }
            if (conversationId != Guid.Empty && payload.ValueKind == JsonValueKind.Object
                && action is "remote.status" or "remote.approve" or "remote.reject" or "remote.revoke" or "remote.grantConversation")
                return await ExpansionRemoteService.RequestAsync(conversationId, action, payload, cancellationToken);
            throw new InvalidDataException("此工作台操作不可用。");
        }
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(action == "media.transcribe" ? TimeSpan.FromMinutes(10) : TimeSpan.FromSeconds(90));
        await ModelGatewayService.EnsureReadyAsync(deadline.Token);
        using var response = await _http.PostAsJsonAsync("/agent/workspace/workbench",
            new { conversationId, mountId, action, payload }, JsonOptions, deadline.Token);
        using var result = await GatewayResponseReader.ReadAsync<JsonDocument>(response,
            "工作台返回为空。", "工作台操作失败（HTTP {0}）。", deadline.Token, JsonOptions);
        return result.RootElement.Clone();
    }

    public void Dispose() => _http.Dispose();
}
