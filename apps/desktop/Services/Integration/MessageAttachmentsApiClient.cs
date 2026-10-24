using System.Net.Http.Json;
using System.Text.Json;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

public sealed record SavedAttachmentEntry(Guid Id, string Name, string ContentType, long SizeBytes,
    Guid ConversationId, Guid MessageId, string ConversationTitle, string Status = "ready",
    string? Sha256 = null, Guid? ProjectId = null)
{
    public ChatAttachment ToAttachment() => new(Id, Name, ContentType, SizeBytes, Status, Sha256);
}
public sealed record SavedAttachmentPage(SavedAttachmentEntry[] Attachments, int Offset = 0,
    int Limit = 50, int Total = 0, bool HasMore = false, int? NextOffset = null);

/// <summary>
/// Imports user-selected files through the gateway; local paths never become model context.
/// 用户选择的文件由网关接收保存；本地来源路径不作为模型上下文或正式附件字段。
/// </summary>
public sealed class MessageAttachmentsApiClient : IDisposable
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private readonly HttpClient _http = GatewayHttpClient.Create(ModelGatewayService.Address, Timeout.InfiniteTimeSpan);
    private sealed record AttachmentResponse(ChatAttachment[] Attachments);

    /// <summary>List committed attachments in the selected chat or its explicitly selected work.</summary>
    /// <remarks>仅列出所选聊天或明确选择的工作中已正式提交的附件，不读取挂载文件夹。</remarks>
    public async Task<SavedAttachmentPage> ListSavedAsync(Guid conversationId, Guid? projectId = null,
        int offset = 0, CancellationToken cancellationToken = default)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(30));
        await ModelGatewayService.EnsureReadyAsync(deadline.Token);
        string endpoint = projectId is Guid project
            ? $"/api/projects/{project:D}/attachments" : $"/api/conversations/{conversationId:D}/attachments";
        using var response = await _http.GetAsync(endpoint + $"?offset={Math.Max(0, offset)}&limit=50", deadline.Token);
        return await GatewayResponseReader.ReadAsync<SavedAttachmentPage>(response,
            UiText.Get("附件列表为空。"), UiText.Get("附件读取失败（HTTP {0}）。"), deadline.Token, JsonOptions);
    }

    public async Task<ChatAttachment[]> ImportAsync(Guid conversationId, Guid messageId,
        IReadOnlyList<MessageAttachmentDraft> files, CancellationToken cancellationToken = default)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromMinutes(5));
        await ModelGatewayService.EnsureReadyAsync(deadline.Token);
        using var response = await _http.PostAsJsonAsync($"/api/conversations/{conversationId:D}/attachments/import",
            new { messageId, files = files.Select(file => new { file.Id, file.Path, file.Name }).ToArray() }, JsonOptions, deadline.Token);
        var result = await GatewayResponseReader.ReadAsync<AttachmentResponse>(response,
            UiText.Get("附件接收结果为空。"), UiText.Get("附件接收失败（HTTP {0}）。"), deadline.Token, JsonOptions);
        // Original reception makes the card usable; extraction readiness is a separate state.
        // 原件接收完成即可打开卡片；正文解析是否完成由后台独立维护。
        return result.Attachments.Select(file => file.Status == "pending" ? file with { Status = "ready" } : file).ToArray();
    }

    public async Task<ChatAttachment[]> ListAsync(Guid conversationId, Guid messageId, CancellationToken cancellationToken = default)
    {
        await ModelGatewayService.EnsureReadyAsync(cancellationToken);
        using var response = await _http.GetAsync($"/api/conversations/{conversationId:D}/attachments?messageId={messageId:D}", cancellationToken);
        return (await GatewayResponseReader.ReadAsync<AttachmentResponse>(response,
            UiText.Get("附件列表为空。"), UiText.Get("附件读取失败（HTTP {0}）。"), cancellationToken, JsonOptions)).Attachments;
    }

    /// <summary>Read image bytes only through the message-scoped attachment endpoint.</summary>
    /// <remarks>图片仅通过消息范围内的附件接口读取；视图不处理原始磁盘路径。</remarks>
    public async Task<(byte[] Bytes, string ContentType)?> ReadImageAsync(Guid conversationId, Guid messageId,
        Guid attachmentId, CancellationToken cancellationToken = default)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(30));
        using var response = await _http.GetAsync(
            $"/api/conversations/{conversationId:D}/attachments/{attachmentId:D}/content?messageId={messageId:D}", deadline.Token);
        if (!response.IsSuccessStatusCode || response.Content.Headers.ContentType?.MediaType is not { } mediaType
            || !mediaType.StartsWith("image/", StringComparison.OrdinalIgnoreCase)) return null;
        return (await response.Content.ReadAsByteArrayAsync(deadline.Token), mediaType);
    }

    /// <summary>Open the saved attachment without exposing application-private disk paths.</summary>
    /// <remarks>打开已保存附件；预览仍由消息范围接口授权，不向工作区文件工具开放私有数据目录。</remarks>
    public async Task<WorkspacePreviewTarget> ReadPreviewAsync(Guid conversationId, Guid messageId,
        ChatAttachment attachment, int offset = 0, CancellationToken cancellationToken = default, string? representation = null)
    {
        if (string.IsNullOrEmpty(attachment.Sha256))
            attachment = (await ListAsync(conversationId, messageId, cancellationToken))
                .FirstOrDefault(item => item.Id == attachment.Id) ?? attachment;
        string endpoint = $"/api/conversations/{conversationId:D}/attachments/{attachment.Id:D}";
        string query = $"messageId={messageId:D}";
        if (!string.IsNullOrEmpty(attachment.Sha256)) query += "&version=" + Uri.EscapeDataString(attachment.Sha256);
        string extension = System.IO.Path.GetExtension(attachment.Name).ToLowerInvariant();
        if (extension is ".doc" or ".docx" or ".ppt" or ".pptx" or ".odt" or ".odp" or ".rtf" or ".xls" or ".xlsx" or ".ods")
        {
            using var previewDeadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
            previewDeadline.CancelAfter(TimeSpan.FromSeconds(150));
            await ModelGatewayService.EnsureReadyAsync(previewDeadline.Token);
            string previewQuery = query + $"&offset={offset}";
            if (representation is not null) previewQuery += "&representation=" + Uri.EscapeDataString(representation);
            using var previewResponse = await _http.GetAsync(endpoint + "/preview?" + previewQuery, previewDeadline.Token);
            return await GatewayResponseReader.ReadAsync<WorkspacePreviewTarget>(previewResponse,
                UiText.Get("附件解析结果为空。"), UiText.Get("附件解析失败（HTTP {0}）。"), previewDeadline.Token, JsonOptions);
        }
        string kind = extension == ".svg" || attachment.ContentType.StartsWith("image/", StringComparison.OrdinalIgnoreCase) ? "image"
            : extension is ".html" or ".htm" or ".xhtml" ? "html"
            : extension == ".pdf" ? "pdf" : extension is ".md" or ".markdown" ? "markdown"
            : extension is ".mmd" or ".mermaid" ? "mermaid" : "text";
        if (kind is "image" or "pdf" or "html")
            return new() { Kind = kind, Title = attachment.Name, Path = attachment.Name,
                Version = attachment.Sha256 ?? attachment.Id.ToString("D"), MimeType = attachment.ContentType,
                ContentUrl = new Uri(_http.BaseAddress!, endpoint + "/content?" + query).AbsoluteUri };
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(90));
        await ModelGatewayService.EnsureReadyAsync(deadline.Token);
        using var response = await _http.GetAsync(endpoint + $"/text?{query}&offset={offset}&maxChars=64000", deadline.Token);
        using var extracted = await GatewayResponseReader.ReadAsync<JsonDocument>(response,
            UiText.Get("附件解析结果为空。"), UiText.Get("附件解析失败（HTTP {0}）。"), deadline.Token, JsonOptions);
        var value = extracted.RootElement;
        string? error = value.TryGetProperty("error", out var errorValue) ? errorValue.GetString() : null;
        return new() { Kind = kind, Title = attachment.Name, Path = attachment.Name,
            Version = value.GetProperty("sourceVersion").GetString() ?? attachment.Id.ToString("D"),
            MimeType = attachment.ContentType, Text = value.GetProperty("text").GetString(), Notice = error,
            HasMore = value.GetProperty("hasMore").GetBoolean(), NextOffset = value.GetProperty("nextOffset").GetInt32() };
    }

    public void Dispose() => _http.Dispose();
}
