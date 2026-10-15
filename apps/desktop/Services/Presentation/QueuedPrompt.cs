using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

/// <summary>
/// A pending user message retains the model and permission selection made when it was queued.
/// 待发送消息保留入队时的模型及权限选择；引导或正常发送共用同一条内容。
/// </summary>
public sealed class QueuedPrompt(Guid id, string text, string? provider, string? model, string permissionMode,
    MessageAttachmentDraft[]? attachmentDrafts = null)
{
    public Guid Id { get; } = id;
    public Guid ReplyMessageId { get; } = Guid.NewGuid();
    public string Text { get; set; } = text;
    public string? Provider { get; } = provider;
    public string? Model { get; } = model;
    public string PermissionMode { get; } = permissionMode;
    public bool IsSubmitting { get; set; }
    public MessageAttachmentDraft[] AttachmentDrafts { get; } = attachmentDrafts ?? [];
    public List<ChatAttachment> Attachments { get; set; } = (attachmentDrafts ?? []).Select(file => file.ToAttachment()).ToList();
    public string DisplayText => AttachmentDrafts.Length == 0 ? Text :
        (string.IsNullOrWhiteSpace(Text) ? "" : Text + "\n") + "📎 " + string.Join("、", AttachmentDrafts.Select(file => file.Name));
}
