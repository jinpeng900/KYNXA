using System.Text.Json;
using System.Text.Json.Serialization;

namespace KYNXA.Contracts;

public static class ChatPermissionModes
{
    public const string Ask = "ask";
    public const string Smart = "smart";
    public const string Full = "full";
    public static bool IsSupported(string? mode) => mode is Ask or Smart or Full;
}

public sealed record ChatRequest(Guid ConversationId, string Message, string? Model = null,
    string PermissionMode = ChatPermissionModes.Ask, string? Provider = null, Guid? RequestId = null, Guid? UserMessageId = null,
    Guid[]? AttachmentIds = null);

/// <summary>
/// Stable attachment references live with the user message; original bytes belong to the gateway.
/// 用户消息保存稳定附件引用；原始文件由网关管理，缩略图草稿不写入正式日志。
/// </summary>
public sealed record ChatAttachment(Guid Id, string Name, string ContentType, long SizeBytes,
    string Status = "ready", string? Sha256 = null, string? Error = null, string? ThumbnailUrl = null)
{
    [JsonIgnore]
    public string? ThumbnailDataUrl { get; init; }
}
public sealed record ChatReply(Guid ConversationId, Guid RequestId, string Role, string Content, DateTimeOffset CreatedAt,
    long DurationMs = 0, JsonElement? TaskCompletion = null);

public sealed record ChatStreamEvent(string Type, Guid ConversationId, Guid RequestId, DateTimeOffset CreatedAt,
    string? Delta = null, string? Content = null, string? Reasoning = null, string? Error = null, ToolActivity? Tool = null,
    AssistantSegment? Segment = null, string? SegmentId = null, AssistantSegment[]? AssistantSegments = null,
    int? ToolStreamProtocol = null, long DurationMs = 0, HostTerminalOutput? Terminal = null, JsonElement? TaskCompletion = null,
    string? ServerId = null, bool? Ready = null, string? Code = null, string? Message = null, Guid? TaskId = null,
    Guid? RootRequestTaskId = null, string? Phase = null, int? Round = null, Guid? ApprovalId = null,
    Guid? ReplyMessageId = null, Guid? UserMessageId = null, Guid? SupersededReplyMessageId = null,
    string? SessionId = null, int? Cols = null, int? Rows = null, string? Title = null, string? Workdir = null,
    Guid? MountId = null, string? MountName = null, string? ToolCallId = null,
    bool? Approved = null, string? Scope = null);

public static class ChatDurationRules
{
    public const long MaximumDurationMs = 9007199254740991;
    public static bool IsValid(long value) => value is >= 0 and <= MaximumDurationMs;
}
