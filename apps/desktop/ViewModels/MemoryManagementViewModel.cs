using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Net;
using System.Runtime.CompilerServices;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;

namespace KYNXA_Desktop.ViewModels;

/// <summary>A saved chat, a real project, or the user scope. The display name is never used as identity.</summary>
public sealed record MemoryTarget(string Scope, Guid? Id, string DisplayName, bool IsArchived = false)
{
    public string ScopeId => Scope == MemoryScopes.User ? "user" : Id?.ToString("D") ?? string.Empty;

    public void Validate()
    {
        if (!MemoryScopes.IsSupported(Scope) || (Scope == MemoryScopes.User ? Id is not null : Id is null || Id == Guid.Empty))
            throw new ArgumentException("A valid saved memory target is required.");
    }
}

public enum MemoryManagementStatus
{
    None, Loading, Ready, Saved, Deleted, Conflict, ConflictRefreshFailed, SavedRefreshFailed,
    DeletedRefreshFailed, Error, InvalidContent, InvalidKind, EntryMissing, WriteOutcomeUnknown, Cancelled
}

public enum MemoryOperationResult
{
    Success, Conflict, ValidationFailed, NotReady, EntryMissing, Failed, Cancelled, Superseded
}

/// <summary>In-memory editor state. Late responses are ignored after a target change or disposal.</summary>
public sealed class MemoryManagementViewModel : INotifyPropertyChanged, IDisposable
{
    private readonly IMemoryApi _api;
    private readonly bool _ownsApi;
    private CancellationTokenSource? _operation;
    private int _generation;
    private int _editorGeneration;
    private bool _disposed;
    private string _editorContent = string.Empty;
    private string _editorKind = MemoryKinds.Fact;
    private string _baselineContent = string.Empty;
    private string _baselineKind = MemoryKinds.Fact;
    private Guid? _editingId;
    private MemoryEntry? _selectedEntry;
    private bool _createOutcomeUnknown;
    private CompletedWrite? _completedWrite;

    public MemoryManagementViewModel() : this(new MemoryApiClient()) { }

    public MemoryManagementViewModel(IMemoryApi api, bool ownsApi = true)
    {
        _api = api;
        _ownsApi = ownsApi;
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    public MemoryTarget? Target { get; private set; }
    public ObservableCollection<MemoryEntry> Entries { get; } = [];
    public ObservableCollection<MemoryEntry> VisibleEntries => Entries;
    public bool IsBusy { get; private set; }
    public bool IsLoaded { get; private set; }
    public long? ScopeRevision { get; private set; }
    public MemoryManagementStatus Status { get; private set; }
    public Exception? Error { get; private set; }
    public Exception? RefreshError { get; private set; }
    public bool IsNew => _editingId is null;
    public bool HasChanges => _editorContent != _baselineContent || _editorKind != _baselineKind;
    public MemoryInputError InputError => MemoryInputValidation.Validate(_editorContent, _editorKind);
    public bool CanSave => !_disposed && !IsBusy && IsLoaded && !_createOutcomeUnknown && InputError == MemoryInputError.None &&
        (IsNew || (HasChanges && Entries.Any(entry => entry.Id == _editingId)));
    public bool CanDelete => !_disposed && !IsBusy && IsLoaded && SelectedEntry is not null &&
        Entries.Any(entry => entry.Id == SelectedEntry.Id);

    public MemoryEntry? SelectedEntry
    {
        get => _selectedEntry;
        set
        {
            if (_selectedEntry == value) return;
            _selectedEntry = value;
            Notify();
            Notify(nameof(CanDelete));
        }
    }

    public string EditorContent
    {
        get => _editorContent;
        set
        {
            value ??= string.Empty;
            if (_editorContent == value) return;
            _editorContent = value;
            _editorGeneration++;
            NotifyEditor();
        }
    }

    public string EditorKind
    {
        get => _editorKind;
        set
        {
            value ??= string.Empty;
            if (_editorKind == value) return;
            _editorKind = value;
            _editorGeneration++;
            NotifyEditor();
        }
    }

    public async Task SelectTargetAsync(MemoryTarget? target, CancellationToken cancellationToken = default)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        target?.Validate();
        CancelOperation();
        Target = target;
        Entries.Clear();
        IsLoaded = false;
        ScopeRevision = null;
        Error = RefreshError = null;
        _completedWrite = null;
        ResetEditor(null);
        Status = MemoryManagementStatus.None;
        NotifyAll();
        if (target is not null) await RefreshAsync(cancellationToken);
    }

    public async Task RefreshAsync(CancellationToken cancellationToken = default)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        if (Target is not { } target || IsBusy) return;
        var operation = StartOperation(cancellationToken);
        int editorGeneration = _editorGeneration;
        bool refreshUnchangedEditor = !HasChanges && _editingId is not null;
        try
        {
            Error = RefreshError = null;
            Status = MemoryManagementStatus.Loading;
            IsLoaded = false;
            NotifyAll();
            var document = await _api.GetAsync(target, operation.Token);
            operation.Token.ThrowIfCancellationRequested();
            if (!IsCurrent(operation)) return;
            ApplyDocument(document);
            if (_completedWrite is { } completed) ApplyCompletedWrite(completed);
            else if (_editingId is not null && !Entries.Any(entry => entry.Id == _editingId)) Status = MemoryManagementStatus.EntryMissing;
            else
            {
                if (refreshUnchangedEditor && _editorGeneration == editorGeneration)
                    ResetEditor(Entries.FirstOrDefault(entry => entry.Id == _editingId));
                Status = _createOutcomeUnknown ? MemoryManagementStatus.WriteOutcomeUnknown : MemoryManagementStatus.Ready;
            }
        }
        catch (OperationCanceledException) when (operation.Token.IsCancellationRequested)
        {
            if (IsCurrent(operation)) Status = MemoryManagementStatus.Cancelled;
        }
        catch (Exception error)
        {
            if (!IsCurrent(operation)) return;
            Error = error;
            Status = MemoryManagementStatus.Error;
        }
        finally { CompleteOperation(operation); }
    }

    public void BeginNew()
    {
        if (_disposed || IsBusy) return;
        ResetEditor(null);
        Error = RefreshError = null;
        Status = IsLoaded ? MemoryManagementStatus.Ready : MemoryManagementStatus.None;
        NotifyAll();
    }

    public void BeginEdit(MemoryEntry entry)
    {
        if (_disposed || IsBusy) return;
        var current = Entries.FirstOrDefault(item => item.Id == entry.Id);
        if (current is null) return;
        ResetEditor(current);
        Error = RefreshError = null;
        Status = MemoryManagementStatus.Ready;
        NotifyAll();
    }

    public Task<MemoryOperationResult> SaveAsync(CancellationToken cancellationToken = default)
    {
        if (_disposed || IsBusy || !IsLoaded || Target is null || ScopeRevision is null || _createOutcomeUnknown)
            return Task.FromResult(MemoryOperationResult.NotReady);
        if (InputError != MemoryInputError.None)
        {
            Status = InputError == MemoryInputError.InvalidKind ? MemoryManagementStatus.InvalidKind : MemoryManagementStatus.InvalidContent;
            NotifyAll();
            return Task.FromResult(MemoryOperationResult.ValidationFailed);
        }
        if (_editingId is not null && !Entries.Any(entry => entry.Id == _editingId))
        {
            Status = MemoryManagementStatus.EntryMissing;
            NotifyAll();
            return Task.FromResult(MemoryOperationResult.EntryMissing);
        }
        if (!CanSave) return Task.FromResult(MemoryOperationResult.NotReady);
        return WriteAsync(deleting: false, cancellationToken);
    }

    public Task<MemoryOperationResult> DeleteAsync(CancellationToken cancellationToken = default) =>
        CanDelete ? WriteAsync(deleting: true, cancellationToken) : Task.FromResult(MemoryOperationResult.NotReady);

    private async Task<MemoryOperationResult> WriteAsync(bool deleting, CancellationToken cancellationToken)
    {
        var target = Target!;
        long revision = ScopeRevision!.Value;
        Guid? entryId = deleting ? SelectedEntry!.Id : _editingId;
        string content = EditorContent, kind = EditorKind;
        int editorGeneration = _editorGeneration;
        var previousIds = Entries.Select(entry => entry.Id).ToHashSet();
        var operation = StartOperation(cancellationToken);
        bool committed = false;
        try
        {
            Error = RefreshError = null;
            NotifyAll();
            MemoryScopeDocument written;
            if (deleting) written = await _api.DeleteAsync(target, entryId!.Value, new(target.Scope, revision), operation.Token);
            else if (entryId is { } id) written = await _api.UpdateAsync(target, id, new(target.Scope, content, kind, revision), operation.Token);
            else written = await _api.CreateAsync(target, new(target.Scope, content, kind, revision), operation.Token);
            committed = true;
            if (!IsCurrent(operation)) return MemoryOperationResult.Superseded;
            if (!deleting && entryId is null)
                entryId = written.Entries.FirstOrDefault(entry => !previousIds.Contains(entry.Id))?.Id;
            _completedWrite = new(entryId, deleting, editorGeneration);
            IsLoaded = false;
            operation.Token.ThrowIfCancellationRequested();
            var document = await _api.GetAsync(target, operation.Token);
            operation.Token.ThrowIfCancellationRequested();
            if (!IsCurrent(operation)) return MemoryOperationResult.Superseded;
            ApplyDocument(document);
            ApplyCompletedWrite(_completedWrite);
            return MemoryOperationResult.Success;
        }
        catch (GatewayApiException error) when (error.StatusCode == HttpStatusCode.Conflict && error.ErrorCode == "MEMORY_CONFLICT" && !committed)
        {
            if (!IsCurrent(operation)) return MemoryOperationResult.Superseded;
            Error = error;
            IsLoaded = false;
            Status = MemoryManagementStatus.Conflict;
            try
            {
                var document = await _api.GetAsync(target, operation.Token);
                operation.Token.ThrowIfCancellationRequested();
                if (!IsCurrent(operation)) return MemoryOperationResult.Superseded;
                ApplyDocument(document);
            }
            catch (Exception refreshError)
            {
                if (!IsCurrent(operation)) return MemoryOperationResult.Superseded;
                RefreshError = refreshError;
                Status = MemoryManagementStatus.ConflictRefreshFailed;
            }
            return MemoryOperationResult.Conflict;
        }
        catch (OperationCanceledException) when (operation.Token.IsCancellationRequested)
        {
            if (!IsCurrent(operation)) return MemoryOperationResult.Superseded;
            IsLoaded = false;
            _createOutcomeUnknown = !deleting && entryId is null && !committed;
            Status = committed ? (deleting ? MemoryManagementStatus.DeletedRefreshFailed : MemoryManagementStatus.SavedRefreshFailed) :
                _createOutcomeUnknown ? MemoryManagementStatus.WriteOutcomeUnknown : MemoryManagementStatus.Cancelled;
            return MemoryOperationResult.Cancelled;
        }
        catch (Exception error)
        {
            if (!IsCurrent(operation)) return MemoryOperationResult.Superseded;
            Error = error;
            if (committed)
            {
                IsLoaded = false;
                Status = deleting ? MemoryManagementStatus.DeletedRefreshFailed : MemoryManagementStatus.SavedRefreshFailed;
            }
            else if (error is not GatewayApiException || (!deleting && entryId is null &&
                error is GatewayApiException { StatusCode: >= HttpStatusCode.InternalServerError }))
            {
                IsLoaded = false;
                _createOutcomeUnknown = !deleting && entryId is null;
                Status = _createOutcomeUnknown ? MemoryManagementStatus.WriteOutcomeUnknown : MemoryManagementStatus.Error;
            }
            else Status = MemoryManagementStatus.Error;
            return MemoryOperationResult.Failed;
        }
        finally { CompleteOperation(operation); }
    }

    private void ApplyDocument(MemoryScopeDocument document)
    {
        // Keep ordering and selection tied to backend IDs, including inactive entries.
        Guid? selectedId = SelectedEntry?.Id;
        Entries.Clear();
        foreach (var entry in document.Entries) Entries.Add(entry);
        SelectedEntry = Entries.FirstOrDefault(entry => entry.Id == selectedId);
        ScopeRevision = document.Revision;
        IsLoaded = true;
    }

    private void ApplyCompletedWrite(CompletedWrite completed)
    {
        if (_editorGeneration == completed.EditorGeneration)
        {
            if (completed.Deleting && _editingId == completed.EntryId) ResetEditor(null);
            else if (!completed.Deleting) ResetEditor(Entries.FirstOrDefault(entry => entry.Id == completed.EntryId));
        }
        Status = completed.Deleting ? MemoryManagementStatus.Deleted : MemoryManagementStatus.Saved;
        _completedWrite = null;
    }

    private void ResetEditor(MemoryEntry? entry)
    {
        SelectedEntry = entry;
        _editingId = entry?.Id;
        _editorContent = _baselineContent = entry?.Content ?? string.Empty;
        _editorKind = _baselineKind = entry?.Kind ?? (Target?.Scope == MemoryScopes.User ? MemoryKinds.Preference : MemoryKinds.Fact);
        _createOutcomeUnknown = false;
        _editorGeneration++;
    }

    private Operation StartOperation(CancellationToken cancellationToken)
    {
        var source = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        _operation = source;
        IsBusy = true;
        return new(source, _generation);
    }

    private bool IsCurrent(Operation operation) => !_disposed && operation.Generation == _generation && ReferenceEquals(_operation, operation.Source);

    private void CompleteOperation(Operation operation)
    {
        if (IsCurrent(operation))
        {
            _operation = null;
            IsBusy = false;
            NotifyAll();
        }
        operation.Source.Dispose();
    }

    private void CancelOperation()
    {
        _generation++;
        var source = _operation;
        _operation = null;
        source?.Cancel();
        IsBusy = false;
    }

    private void Notify([CallerMemberName] string? propertyName = null) => PropertyChanged?.Invoke(this, new(propertyName));

    private void NotifyEditor()
    {
        Notify(nameof(EditorContent));
        Notify(nameof(EditorKind));
        Notify(nameof(HasChanges));
        Notify(nameof(InputError));
        Notify(nameof(CanSave));
    }

    private void NotifyAll() => Notify(string.Empty);

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        CancelOperation();
        if (_ownsApi && _api is IDisposable disposable) disposable.Dispose();
    }

    private sealed record Operation(CancellationTokenSource Source, int Generation)
    {
        public CancellationToken Token => Source.Token;
    }

    private sealed record CompletedWrite(Guid? EntryId, bool Deleting, int EditorGeneration);
}
