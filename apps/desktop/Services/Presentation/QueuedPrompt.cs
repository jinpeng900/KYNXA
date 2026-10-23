using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

/// <summary>
/// A pending user message retains the model and permission selection made when it was queued.
/// 待发送消息保留入队时的模型及权限选择；引导或正常发送共用同一条内容。
/// </summary>
public sealed class QueuedPrompt(Guid id, string text, string? provider, string? model, string permissionMode,
    MessageAttachmentDraft[]? attachmentDrafts = null, Guid? replyMessageId = null)
{
    public Guid Id { get; } = id;
    public Guid ReplyMessageId { get; } = replyMessageId ?? Guid.NewGuid();
    public string Text { get; set; } = text;
    public string? Provider { get; } = provider;
    public string? Model { get; } = model;
    public string PermissionMode { get; } = permissionMode;
    public bool IsSubmitting { get; set; }
    // Every guidance keeps its original task/request across rejected or uncertain submissions.
    // 所有引导均保留原任务和请求身份；提交被拒或结果不明时也不能改发新根任务。
    public bool IsTaskGuidance { get; internal set; }
    public bool SteeringDeliveryUnconfirmed { get; internal set; }
    public Guid? TargetTaskId { get; internal set; }
    public Guid? TargetRequestId { get; internal set; }
    public MessageAttachmentDraft[] AttachmentDrafts { get; } = attachmentDrafts ?? [];
    public List<ChatAttachment> Attachments { get; set; } = (attachmentDrafts ?? []).Select(file => file.ToAttachment()).ToList();
    public string DisplayText => AttachmentDrafts.Length == 0 ? Text :
        (string.IsNullOrWhiteSpace(Text) ? "" : Text + "\n") + "📎 " + string.Join("、", AttachmentDrafts.Select(file => file.Name));
}
