using KYNXA.Contracts;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// 标识本地查看器选中的正式归档工具结果。
/// </summary>
public sealed record ToolResultRequest(Guid ConversationId, Guid MessageId, ToolActivity Tool);
