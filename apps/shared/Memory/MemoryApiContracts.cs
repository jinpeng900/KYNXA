namespace KYNXA.Contracts;

public static class MemoryScopes
{
    public const string Chat = "chat";
    public const string Project = "project";
    public const string User = "user";
    public static bool IsSupported(string? scope) => scope is Chat or Project or User;
}

public static class MemoryKinds
{
    public const string Fact = "fact";
    public const string Preference = "preference";
    public const string Decision = "decision";
    public static bool IsSupported(string? kind) => kind is Fact or Preference or Decision;
}

public enum MemoryInputError
{
    None, EmptyContent, ContentTooLong, ContentContainsNull, InvalidKind
}

/// <summary>Matches the gateway's UTF-16 content limit without changing the user's editor text.</summary>
/// <remarks>与网关的 UTF-16 内容长度限制保持一致，不改写用户的编辑文本。</remarks>
public static class MemoryInputValidation
{
    public const int MaximumContentLength = 4000;
    public const long MaximumRevision = 9007199254740991;

    public static MemoryInputError Validate(string? content, string? kind)
    {
        if (string.IsNullOrWhiteSpace(content)) return MemoryInputError.EmptyContent;
        if (content.Length > MaximumContentLength) return MemoryInputError.ContentTooLong;
        if (content.Contains('\0')) return MemoryInputError.ContentContainsNull;
        return MemoryKinds.IsSupported(kind) ? MemoryInputError.None : MemoryInputError.InvalidKind;
    }
}

public sealed record MemorySource(string Type, string Role, Guid? ConversationId = null, Guid? MessageId = null);
public sealed record MemoryDismissedSource(Guid ConversationId, Guid MessageId, DateTimeOffset DeletedAt);

public sealed record MemoryEntry(Guid Id, string Scope, string ScopeId, string Content, string Kind,
    string Status, MemorySource Source, long Revision, DateTimeOffset CreatedAt, DateTimeOffset UpdatedAt,
    bool? Active = null, bool? SourceAvailable = null, bool? SourceArchived = null);

public sealed record MemoryScopeDocument(int SchemaVersion, string Scope, string ScopeId, long Revision,
    MemoryEntry[] Entries, MemoryDismissedSource[]? DismissedSources = null);

public sealed record ConversationMemoryResponse(Guid ConversationId, Guid? ProjectId,
    bool IsFolderlessWorkspace, MemoryScopeDocument[] Scopes);

public sealed record MemoryCreateRequest(string Scope, string Content, string Kind, long ExpectedRevision);
public sealed record MemoryUpdateRequest(string Scope, string Content, string Kind, long ExpectedRevision);
public sealed record MemoryDeleteRequest(string Scope, long ExpectedRevision);
