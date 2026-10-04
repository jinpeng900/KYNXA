using KYNXA.Contracts;

namespace KYNXA_Desktop.Controls;

/// <summary>Identifies a formally recorded result selected by a local viewer.</summary>
public sealed record ToolResultRequest(Guid ConversationId, Guid MessageId, ToolActivity Tool);
