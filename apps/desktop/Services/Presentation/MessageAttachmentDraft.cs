using KYNXA.Contracts;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Local selection belongs to the composer or queue until the gateway snapshots it.
/// 本地选择只属于输入区或等待队列；正式消息仅保存网关接受的附件元信息。
/// </summary>
public sealed record MessageAttachmentDraft(Guid Id, string Path, string Name, string ContentType,
    long SizeBytes, bool IsTemporary = false)
{
    public ImageSource? Thumbnail { get; set; }
    public string? ThumbnailDataUrl { get; set; }
    public ChatAttachment ToAttachment() => new(Id, Name, ContentType, SizeBytes, "pending") { ThumbnailDataUrl = ThumbnailDataUrl };
}
