using System.Net;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;

namespace MemoryUiSmoke;

/// <summary>A deterministic backend fixture. Every entry and target is synthetic.</summary>
internal sealed class FakeMemoryApi : IMemoryApi, IDisposable
{
    private readonly Dictionary<string, MemoryScopeDocument> _documents = [];
    private readonly Dictionary<string, Queue<TaskCompletionSource<MemoryScopeDocument>>> _delayed = [];
    private readonly Dictionary<string, Exception> _readFailures = [];
    public List<string> Requests { get; } = [];
    public Exception? NextWriteFailure { get; set; }
    public bool ConflictNextWrite { get; set; }
    public bool IsDisposed { get; private set; }
    public int Writes { get; private set; }

    public void Seed(MemoryTarget target, params MemoryEntry[] entries) =>
        _documents[Key(target)] = new(1, target.Scope, target.ScopeId, 1, entries);

    public MemoryScopeDocument Snapshot(MemoryTarget target) => _documents[Key(target)];

    public void FailNextRead(MemoryTarget target, Exception error) => _readFailures[Key(target)] = error;

    public TaskCompletionSource<MemoryScopeDocument> DelayNextRead(MemoryTarget target)
    {
        var completion = new TaskCompletionSource<MemoryScopeDocument>(TaskCreationOptions.RunContinuationsAsynchronously);
        if (!_delayed.TryGetValue(Key(target), out var queue)) _delayed[Key(target)] = queue = new();
        queue.Enqueue(completion);
        return completion;
    }

    public Task<MemoryScopeDocument> GetAsync(MemoryTarget target, CancellationToken cancellationToken = default)
    {
        Requests.Add("GET " + Key(target));
        if (_delayed.TryGetValue(Key(target), out var queue) && queue.TryDequeue(out var delayed))
            // Deliberately model an upstream implementation that returns after cancellation.
            return delayed.Task;
        cancellationToken.ThrowIfCancellationRequested();
        if (_readFailures.Remove(Key(target), out var failure)) return Task.FromException<MemoryScopeDocument>(failure);
        return Task.FromResult(Snapshot(target));
    }

    public Task<MemoryScopeDocument> CreateAsync(MemoryTarget target, MemoryCreateRequest request,
        CancellationToken cancellationToken = default)
    {
        var document = BeforeWrite(target, request.ExpectedRevision, cancellationToken);
        var entry = Entry(target, request.Content, request.Kind) with { Revision = document.Revision + 1 };
        return FinishWrite(target, document, [.. document.Entries, entry]);
    }

    public Task<MemoryScopeDocument> UpdateAsync(MemoryTarget target, Guid entryId, MemoryUpdateRequest request,
        CancellationToken cancellationToken = default)
    {
        var document = BeforeWrite(target, request.ExpectedRevision, cancellationToken);
        var entries = document.Entries.Select(entry => entry.Id == entryId
            ? entry with { Content = request.Content, Kind = request.Kind, Revision = entry.Revision + 1,
                UpdatedAt = DateTimeOffset.UtcNow } : entry).ToArray();
        return FinishWrite(target, document, entries);
    }

    public Task<MemoryScopeDocument> DeleteAsync(MemoryTarget target, Guid entryId, MemoryDeleteRequest request,
        CancellationToken cancellationToken = default)
    {
        var document = BeforeWrite(target, request.ExpectedRevision, cancellationToken);
        return FinishWrite(target, document, document.Entries.Where(entry => entry.Id != entryId).ToArray());
    }

    private MemoryScopeDocument BeforeWrite(MemoryTarget target, long expectedRevision, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        Requests.Add("WRITE " + Key(target));
        var document = Snapshot(target);
        if (NextWriteFailure is { } failure)
        {
            NextWriteFailure = null;
            throw failure;
        }
        if (ConflictNextWrite)
        {
            ConflictNextWrite = false;
            document = document with
            {
                Revision = document.Revision + 1,
                Entries = document.Entries.Select((entry, index) => index == 0
                    ? entry with { Content = "SERVER_CONCURRENT_EDIT 已由另一窗口修改", Revision = entry.Revision + 1 }
                    : entry).ToArray()
            };
            _documents[Key(target)] = document;
            throw new GatewayApiException("Synthetic revision conflict", HttpStatusCode.Conflict, "MEMORY_CONFLICT");
        }
        if (expectedRevision != document.Revision)
            throw new GatewayApiException("Synthetic stale revision", HttpStatusCode.Conflict, "MEMORY_CONFLICT");
        return document;
    }

    private Task<MemoryScopeDocument> FinishWrite(MemoryTarget target, MemoryScopeDocument previous, MemoryEntry[] entries)
    {
        Writes++;
        var document = previous with { Revision = previous.Revision + 1, Entries = entries };
        _documents[Key(target)] = document;
        return Task.FromResult(document);
    }

    public static MemoryEntry Entry(MemoryTarget target, string content, string kind = MemoryKinds.Fact,
        bool active = true, bool available = true, bool archived = false, bool fromMessage = false) =>
        new(Guid.NewGuid(), target.Scope, target.ScopeId, content, kind, "confirmed",
            fromMessage ? new("user-message", "user", Guid.NewGuid(), Guid.NewGuid()) : new("manual", "user"),
            1, DateTimeOffset.Parse("2026-01-01T00:00:00Z"), DateTimeOffset.Parse("2026-01-01T00:00:00Z"),
            active, available, archived);

    private static string Key(MemoryTarget target) => target.Scope + ":" + target.ScopeId;
    public void Dispose() => IsDisposed = true;
}
