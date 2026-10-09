using System.Text.Json.Serialization;

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

public static class MemoryStatuses
{
    public const string Draft = "draft";
    public const string Confirmed = "confirmed";
    public static bool IsSupported(string? status) => status is Draft or Confirmed;
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
public sealed record MemoryDismissedSource(Guid ConversationId, Guid MessageId, DateTimeOffset DeletedAt,
    string? CandidateFingerprint = null, Guid? CandidateBatchThroughMessageId = null);

/// <summary>Draft provenance contains saved user quotations and grants no confirmation or broader scope.</summary>
/// <remarks>草稿来源仅保存正式用户原话，不能自行授予确认状态或更大范围。</remarks>
public sealed record MemoryCandidateQuote(Guid MessageId, string Text);
public sealed record MemoryCandidate(string Algorithm, string Trigger, string Fingerprint,
    MemoryCandidateQuote[] Quotes, Guid? BatchThroughMessageId = null);

public sealed record MemoryEntry(Guid Id, string Scope, string ScopeId, string Content, string Kind,
    string Status, MemorySource Source, long Revision, DateTimeOffset CreatedAt, DateTimeOffset UpdatedAt,
    bool? Active = null, bool? SourceAvailable = null, bool? SourceArchived = null, MemoryCandidate? Candidate = null);

public sealed record MemoryScopeDocument(int SchemaVersion, string Scope, string ScopeId, long Revision,
    MemoryEntry[] Entries, MemoryDismissedSource[]? DismissedSources = null,
    MemoryCandidateSettings? CandidateSettings = null, long? CandidateSettingsRevision = null);

public sealed record ConversationMemoryResponse(Guid ConversationId, Guid? ProjectId,
    bool IsFolderlessWorkspace, MemoryScopeDocument[] Scopes);

public sealed record MemoryCreateRequest(string Scope, string Content, string Kind, long ExpectedRevision);
public sealed record MemoryUpdateRequest(string Scope, string Content, string Kind, long ExpectedRevision,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Status = null);
public sealed record MemoryConfirmRequest(string Scope, long ExpectedRevision, string Status = MemoryStatuses.Confirmed);
public sealed record MemoryDeleteRequest(string Scope, long ExpectedRevision);

public sealed record MemoryCandidateSettings(bool Enabled = true, int MinTurns = 6, int MaxTurns = 12,
    int MinTokens = 2048, int MaxTokens = 4096, int MaxCandidates = 8);

public sealed record MemoryCandidateSettingsPatch(
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] bool? Enabled = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? MinTurns = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? MaxTurns = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? MinTokens = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? MaxTokens = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? MaxCandidates = null);

public sealed record MemoryCandidateSettingsRequest(long ExpectedRevision, MemoryCandidateSettingsPatch Patch);
public sealed record MemoryCandidateResult(int Created, int? Deduplicated = null, string? Reason = null,
    int? CompleteTurns = null, int? EstimatedTokens = null, string? Code = null);
public sealed record MemoryCandidateChangeError(string Code);
public sealed record MemoryCandidateStatus(MemoryCandidateSettings Settings, long SettingsRevision, int PendingTasks,
    string Algorithm, MemoryCandidateResult? LastResult = null, MemoryCandidateChangeError? ChangeNotificationError = null);
