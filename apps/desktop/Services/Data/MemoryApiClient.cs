using System.Net.Http.Json;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.ViewModels;

namespace KYNXA_Desktop.Services;

public interface IMemoryApi
{
    Task<MemoryScopeDocument> GetAsync(MemoryTarget target, CancellationToken cancellationToken = default);
    Task<MemoryScopeDocument> CreateAsync(MemoryTarget target, MemoryCreateRequest request, CancellationToken cancellationToken = default);
    Task<MemoryScopeDocument> UpdateAsync(MemoryTarget target, Guid entryId, MemoryUpdateRequest request, CancellationToken cancellationToken = default);
    Task<MemoryScopeDocument> ConfirmAsync(MemoryTarget target, Guid entryId, MemoryConfirmRequest request, CancellationToken cancellationToken = default);
    Task<MemoryScopeDocument> DeleteAsync(MemoryTarget target, Guid entryId, MemoryDeleteRequest request, CancellationToken cancellationToken = default);
}

/// <summary>
/// Gateway memory transport; no drafts, snapshots, or authoritative data are written by the desktop.
/// 记忆 API 传输层；桌面不写入草稿快照或正式记忆数据。
/// </summary>
public sealed class MemoryApiClient : IMemoryApi, IDisposable
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private readonly HttpClient _httpClient;
    private readonly Func<CancellationToken, Task> _ensureGatewayReady;
    private readonly bool _ownsHttpClient;

    public MemoryApiClient() : this(new HttpClient
    {
        BaseAddress = ModelGatewayService.Address,
        Timeout = TimeSpan.FromSeconds(30)
    }, ModelGatewayService.EnsureReadyAsync, ownsHttpClient: true) { }

    public MemoryApiClient(HttpClient http, Func<CancellationToken, Task>? ensureReady = null, bool ownsHttpClient = false)
    {
        _httpClient = http;
        _ensureGatewayReady = ensureReady ?? (_ => Task.CompletedTask);
        _ownsHttpClient = ownsHttpClient;
    }

    public async Task<MemoryScopeDocument> GetAsync(MemoryTarget target, CancellationToken cancellationToken = default)
    {
        string path = PathFor(target);
        await _ensureGatewayReady(cancellationToken);
        using var response = await _httpClient.GetAsync(path, cancellationToken);
        var payload = await ReadAsync<JsonElement>(response, cancellationToken);
        JsonElement rawDocument = payload;
        MemoryScopeDocument document;
        if (target.Scope == MemoryScopes.Chat)
        {
            var snapshot = payload.Deserialize<ConversationMemoryResponse>(JsonOptions) ?? throw InvalidResponse();
            if (snapshot.ConversationId != target.Id || snapshot.Scopes is null)
                throw new InvalidDataException(UiText.Get("记忆接口返回了空响应。"));
            document = snapshot.Scopes.SingleOrDefault(scope => scope.Scope == target.Scope && scope.ScopeId == target.ScopeId)
                ?? throw new InvalidDataException(UiText.Get("记忆接口返回了空响应。"));
            rawDocument = Field(payload, "scopes").EnumerateArray().Single(scope =>
                Text(scope, "scope") == target.Scope && Text(scope, "scopeId") == target.ScopeId);
        }
        else document = payload.Deserialize<MemoryScopeDocument>(JsonOptions) ?? throw InvalidResponse();
        ValidateDocument(target, document, rawDocument, requireSourceStatus: true);
        return document;
    }

    public Task<MemoryScopeDocument> CreateAsync(MemoryTarget target, MemoryCreateRequest request, CancellationToken cancellationToken = default)
    {
        ValidateInput(target, request.Scope, request.ExpectedRevision, request.Content, request.Kind);
        return WriteAsync(target, HttpMethod.Post, PathFor(target), request, cancellationToken);
    }

    public Task<MemoryScopeDocument> UpdateAsync(MemoryTarget target, Guid entryId, MemoryUpdateRequest request, CancellationToken cancellationToken = default)
    {
        ValidateInput(target, request.Scope, request.ExpectedRevision, request.Content, request.Kind);
        if (request.Status is not null && !MemoryStatuses.IsSupported(request.Status))
            throw new ArgumentException("Unsupported memory status.", nameof(request));
        return WriteAsync(target, HttpMethod.Patch, EntryPathFor(target, entryId), request, cancellationToken);
    }

    public Task<MemoryScopeDocument> ConfirmAsync(MemoryTarget target, Guid entryId, MemoryConfirmRequest request, CancellationToken cancellationToken = default)
    {
        ValidateScopeRevision(target, request.Scope, request.ExpectedRevision);
        if (request.Status != MemoryStatuses.Confirmed || request.ProposalAction is not (null or "add" or "update" or "delete"))
            throw new ArgumentException("An explicit supported confirmation is required.", nameof(request));
        return WriteAsync(target, HttpMethod.Patch, EntryPathFor(target, entryId), request, cancellationToken);
    }

    public Task<MemoryScopeDocument> DeleteAsync(MemoryTarget target, Guid entryId, MemoryDeleteRequest request, CancellationToken cancellationToken = default)
    {
        ValidateScopeRevision(target, request.Scope, request.ExpectedRevision);
        return WriteAsync(target, HttpMethod.Delete, EntryPathFor(target, entryId), request, cancellationToken);
    }

    private async Task<MemoryScopeDocument> WriteAsync<T>(MemoryTarget target, HttpMethod method, string path, T payload,
        CancellationToken cancellationToken)
    {
        await _ensureGatewayReady(cancellationToken);
        using var request = new HttpRequestMessage(method, path) { Content = JsonContent.Create(payload, options: JsonOptions) };
        using var response = await _httpClient.SendAsync(request, cancellationToken);
        var responsePayload = await ReadAsync<JsonElement>(response, cancellationToken);
        var document = responsePayload.Deserialize<MemoryScopeDocument>(JsonOptions) ?? throw InvalidResponse();
        ValidateDocument(target, document, responsePayload, requireSourceStatus: false);
        return document;
    }

    private static string PathFor(MemoryTarget target)
    {
        target.Validate();
        return target.Scope switch
        {
            MemoryScopes.Chat => $"/api/conversations/{target.ScopeId}/memory",
            MemoryScopes.Project => $"/api/projects/{target.ScopeId}/memory",
            MemoryScopes.User => "/api/memory/user",
            _ => throw new ArgumentException("Unsupported memory scope.", nameof(target))
        };
    }

    private static string EntryPathFor(MemoryTarget target, Guid entryId)
    {
        if (entryId == Guid.Empty) throw new ArgumentException("A saved memory entry ID is required.", nameof(entryId));
        return $"{PathFor(target)}/{entryId:D}";
    }

    private static void ValidateScopeRevision(MemoryTarget target, string scope, long revision)
    {
        target.Validate();
        if (scope != target.Scope) throw new ArgumentException("Memory scope must match its target.", nameof(scope));
        if (revision is < 0 or > MemoryInputValidation.MaximumRevision)
            throw new ArgumentOutOfRangeException(nameof(revision));
    }

    private static void ValidateInput(MemoryTarget target, string scope, long revision, string content, string kind)
    {
        ValidateScopeRevision(target, scope, revision);
        var error = MemoryInputValidation.Validate(content, kind);
        if (error != MemoryInputError.None) throw new ArgumentException($"Invalid memory input: {error}.", nameof(content));
    }

    private static void ValidateDocument(MemoryTarget target, MemoryScopeDocument document, JsonElement rawDocument, bool requireSourceStatus)
    {
        if (document.SchemaVersion != 1 || document.Scope != target.Scope || document.ScopeId != target.ScopeId ||
            document.Revision is < 0 or > MemoryInputValidation.MaximumRevision || document.Entries is null)
            throw new InvalidDataException(UiText.Get("记忆接口返回了空响应。"));
        var entryIds = new HashSet<Guid>();
        var rawEntries = Field(rawDocument, "entries").EnumerateArray().ToArray();
        if (rawEntries.Length != document.Entries.Length) throw InvalidResponse();
        for (int index = 0; index < document.Entries.Length; index++)
        {
            var entry = document.Entries[index];
            if (entry is null || entry.Id == Guid.Empty || !entryIds.Add(entry.Id) || entry.Scope != document.Scope ||
                entry.ScopeId != document.ScopeId || !MemoryStatuses.IsSupported(entry.Status) ||
                entry.Revision < 1 || entry.Revision > document.Revision ||
                MemoryInputValidation.Validate(entry.Content, entry.Kind) != MemoryInputError.None || entry.Source is null ||
                entry.Source.Role != "user" || entry.Source.Type is not ("manual" or "user-message") ||
                entry.Source.ConversationId == Guid.Empty || entry.Source.MessageId == Guid.Empty ||
                (entry.Source.Type == "manual" && entry.Source.MessageId is not null) ||
                (entry.Source.Type == "user-message" && (entry.Source.ConversationId is null || entry.Source.MessageId is null)) ||
                (entry.Status == MemoryStatuses.Draft && entry.Active == true) ||
                (requireSourceStatus && (entry.Active is null || entry.SourceAvailable is null || entry.SourceArchived is null)))
                throw new InvalidDataException(UiText.Get("记忆接口返回了空响应。"));
            ValidateProvenance(entry, document);
            ValidateRawProvenance(entry, rawEntries[index]);
        }
    }

    private static void ValidateProvenance(MemoryEntry entry, MemoryScopeDocument document)
    {
        // Transport validation preserves draft authority and bounds provenance shown by the editor.
        // 传输校验保留草稿确认边界，并限制编辑器展示的来源信息。
        bool invalid = entry.Status == MemoryStatuses.Draft && entry.Active == true;
        if (entry.Candidate is { } candidate)
            invalid |= entry.Source.Type != "user-message" || !ValidQuotes(candidate.Quotes, entry.Source.MessageId);
        if (entry.Proposal is { } proposal)
        {
            invalid |= entry.Source.Type != "user-message" || proposal.Action is not ("add" or "update" or "delete") ||
                !ValidQuotes(proposal.Quotes, entry.Source.MessageId) || string.IsNullOrWhiteSpace(proposal.Reason) ||
                proposal.Reason.Length > 1000 || proposal.Conflicts is null || proposal.Conflicts.Length > 8 ||
                proposal.Conflicts.Any(conflict => string.IsNullOrWhiteSpace(conflict) || conflict.Length > 500) ||
                (proposal.Action == "add" ? proposal.Target is not null : proposal.Target is null);
            if (proposal.Target is { } target)
                invalid |= target.Id == Guid.Empty || target.Scope != document.Scope || target.ScopeId != document.ScopeId ||
                    target.ExpectedRevision is < 0 or > MemoryInputValidation.MaximumRevision ||
                    target.EntryRevision is < 1 or > MemoryInputValidation.MaximumRevision ||
                    string.IsNullOrWhiteSpace(target.Identity);
        }
        if (invalid) throw new InvalidDataException(UiText.Get("记忆接口返回了空响应。"));
    }

    private static bool ValidQuotes(MemoryCandidateQuote[]? quotes, Guid? sourceMessageId) =>
        quotes is { Length: > 0 and <= 12 } && quotes.All(quote => quote is not null && quote.MessageId != Guid.Empty &&
            !string.IsNullOrWhiteSpace(quote.Text) && quote.Text.Length <= MemoryInputValidation.MaximumContentLength &&
            !quote.Text.Contains('\0')) && quotes.Select(quote => quote.MessageId).Distinct().Count() == quotes.Length &&
        quotes.Any(quote => quote.MessageId == sourceMessageId);

    // Validate raw provenance as well as the typed editor contract before showing confirmation actions.
    // 展示确认操作前，同时核验原始来源字段和编辑器的强类型合同。
    private static void ValidateRawProvenance(MemoryEntry entry, JsonElement rawEntry)
    {
        var candidate = Field(rawEntry, "candidate");
        var proposal = Field(rawEntry, "proposal");
        bool hasCandidate = candidate.ValueKind is not (JsonValueKind.Null or JsonValueKind.Undefined);
        bool hasProposal = proposal.ValueKind is not (JsonValueKind.Null or JsonValueKind.Undefined);
        if (!OnlyFields(Field(rawEntry, "source"), "type", "role", "conversationId", "messageId") ||
            hasCandidate && hasProposal || entry.Status == MemoryStatuses.Draft && !hasCandidate && !hasProposal)
            throw InvalidResponse();
        if (hasCandidate)
        {
            string? trigger = Text(candidate, "trigger");
            var cursor = Field(candidate, "batchThroughMessageId");
            if (entry.Source.Type != "user-message" ||
                !OnlyFields(candidate, "algorithm", "trigger", "fingerprint", "quotes", "batchThroughMessageId") ||
                Text(candidate, "algorithm") != "conservative-extractive-v1" ||
                trigger is not ("remember" or "correction" or "decision" or "task-complete" or "ordinary-batch") ||
                !Fingerprint(candidate, "fingerprint") || !ValidQuotes(Field(candidate, "quotes"), entry.Source) ||
                cursor.ValueKind is not (JsonValueKind.Null or JsonValueKind.Undefined) && (trigger != "ordinary-batch" || !ValidId(cursor)))
                throw InvalidResponse();
        }
        if (hasProposal)
        {
            string? action = Text(proposal, "action");
            var target = Field(proposal, "target");
            var applied = Field(proposal, "appliedAt");
            var conflicts = Field(proposal, "conflicts");
            bool targetPresent = target.ValueKind is not (JsonValueKind.Null or JsonValueKind.Undefined);
            bool hasAppliedAt = applied.ValueKind is not (JsonValueKind.Null or JsonValueKind.Undefined);
            if (entry.Source.Type != "user-message" ||
                !OnlyFields(proposal, "algorithm", "action", "quotes", "reason", "isInference", "conflicts", "target", "fingerprint", "appliedAt") ||
                Text(proposal, "algorithm") != "main-model-tool-v1" || action is not ("add" or "update" or "delete") ||
                !BoundedText(Text(proposal, "reason"), 1000) || Field(proposal, "isInference").ValueKind is not (JsonValueKind.True or JsonValueKind.False) ||
                !Fingerprint(proposal, "fingerprint") || !ValidQuotes(Field(proposal, "quotes"), entry.Source) ||
                conflicts.ValueKind != JsonValueKind.Array || conflicts.GetArrayLength() > 8 ||
                conflicts.EnumerateArray().Any(value => value.ValueKind != JsonValueKind.String || !BoundedText(value.GetString(), 500)) ||
                (action != "add") != targetPresent || targetPresent && !ValidProposalTarget(target, entry) ||
                hasAppliedAt && (entry.Status == MemoryStatuses.Draft || action != "update" ||
                    applied.ValueKind != JsonValueKind.String || !DateTimeOffset.TryParse(applied.GetString(), out _)) ||
                entry.Status == MemoryStatuses.Confirmed && (action == "delete" || action == "update" && !hasAppliedAt))
                throw InvalidResponse();
        }
    }

    private static bool ValidProposalTarget(JsonElement target, MemoryEntry entry) =>
        OnlyFields(target, "id", "scope", "scopeId", "expectedRevision", "entryRevision", "identity") &&
        ValidId(Field(target, "id")) && Text(target, "scope") == entry.Scope && Text(target, "scopeId") == entry.ScopeId &&
        ValidRevision(Field(target, "expectedRevision"), minimum: 0) && ValidRevision(Field(target, "entryRevision"), minimum: 1) &&
        Fingerprint(target, "identity");

    private static bool ValidQuotes(JsonElement quotes, MemorySource source)
    {
        if (quotes.ValueKind != JsonValueKind.Array || quotes.GetArrayLength() is < 1 or > 12) return false;
        var messageIds = new HashSet<Guid>();
        foreach (var quote in quotes.EnumerateArray())
        {
            if (!OnlyFields(quote, "messageId", "text") || !ValidId(Field(quote, "messageId")) ||
                !messageIds.Add(Field(quote, "messageId").GetGuid()) || !BoundedText(Text(quote, "text"), MemoryInputValidation.MaximumContentLength))
                return false;
        }
        return source.MessageId is { } id && messageIds.Contains(id);
    }

    private static bool BoundedText(string? value, int maximumLength) =>
        !string.IsNullOrWhiteSpace(value) && value.Length <= maximumLength && !value.Contains('\0');

    private static bool Fingerprint(JsonElement value, string name) => Text(value, name) is { Length: 64 } fingerprint &&
        fingerprint.All(character => character is >= '0' and <= '9' or >= 'a' and <= 'f');

    private static bool ValidId(JsonElement value) => value.ValueKind == JsonValueKind.String && value.TryGetGuid(out var id) && id != Guid.Empty;
    private static bool ValidRevision(JsonElement value, long minimum) => value.ValueKind == JsonValueKind.Number &&
        value.TryGetInt64(out long revision) && revision >= minimum && revision <= MemoryInputValidation.MaximumRevision;
    private static bool OnlyFields(JsonElement value, params string[] names) => value.ValueKind == JsonValueKind.Object &&
        value.EnumerateObject().All(property => names.Contains(property.Name, StringComparer.OrdinalIgnoreCase));
    private static JsonElement Field(JsonElement value, string name) => value.ValueKind == JsonValueKind.Object
        ? value.EnumerateObject().LastOrDefault(property => property.Name.Equals(name, StringComparison.OrdinalIgnoreCase)).Value : default;
    private static string? Text(JsonElement value, string name) => Field(value, name) is { ValueKind: JsonValueKind.String } field ? field.GetString() : null;
    private static InvalidDataException InvalidResponse() => new(UiText.Get("记忆接口返回了空响应。"));

    private static Task<T> ReadAsync<T>(HttpResponseMessage response, CancellationToken cancellationToken) =>
        GatewayResponseReader.ReadAsync<T>(response, UiText.Get("记忆接口返回了空响应。"),
            UiText.Get("记忆接口返回 HTTP {0}。"), cancellationToken, JsonOptions);

    public void Dispose()
    {
        if (_ownsHttpClient) _httpClient.Dispose();
    }
}
