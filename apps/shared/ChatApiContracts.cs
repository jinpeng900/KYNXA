namespace KYNXA.Contracts;

public static class ChatPermissionModes
{
    public const string Ask = "ask";
    public const string Smart = "smart";
    public const string Full = "full";
    public static bool IsSupported(string? mode) => mode is Ask or Smart or Full;
}

public sealed record ChatRequest(Guid ConversationId, string Message, string? Model = null,
    string PermissionMode = ChatPermissionModes.Ask, string? Provider = null);
public sealed record ChatReply(Guid ConversationId, Guid RequestId, string Role, string Content, DateTimeOffset CreatedAt);
