namespace KYNXA_Desktop.Services;

/// <summary>Key is a logical call identity, not a mutable result-reference or output version.</summary>
public sealed record ConversationWorkTab(string Key, string Kind, string Title);

/// <summary>Per-conversation display state. Closing never deletes or cancels the source resource.</summary>
public sealed class ConversationWorkTabState
{
    public const int MaximumConversations = 16;
    public const int MaximumAbsentClosedTabs = 256;
    private sealed class Tabs
    {
        public ConversationWorkTab[] Items { get; set; } = [];
        public Dictionary<string, long> Closed { get; } = new(StringComparer.Ordinal);
        public string? SelectedKey { get; set; }
        public long Touched { get; set; }
    }

    private readonly Dictionary<Guid, Tabs> _chats = [];
    private long _version;
    private Tabs? Current => ConversationId is Guid id && _chats.TryGetValue(id, out var tabs) ? tabs : null;
    public Guid? ConversationId { get; private set; }
    public IReadOnlyList<ConversationWorkTab> Items => Current?.Items ?? [];
    public IReadOnlyList<ConversationWorkTab> OpenItems => Current is { } tabs ? tabs.Items.Where(item => !tabs.Closed.ContainsKey(item.Key)).ToArray() : [];
    public ConversationWorkTab? SelectedTab => Current is { } tabs ? tabs.Items.FirstOrDefault(item => item.Key == tabs.SelectedKey && !tabs.Closed.ContainsKey(item.Key)) : null;
    public bool HasItems => Items.Count > 0;
    public bool HasOpenTabs => SelectedTab is not null;

    public bool ShowConversation(Guid? conversationId, IReadOnlyList<ConversationWorkTab> items, bool autoOpenNew = true)
    {
        ArgumentNullException.ThrowIfNull(items);
        ConversationId = conversationId is Guid id && id != Guid.Empty ? id : null;
        if (ConversationId is not Guid currentId) return false;
        if (!_chats.TryGetValue(currentId, out var tabs)) _chats[currentId] = tabs = new();
        tabs.Touched = ++_version;
        while (_chats.Count > MaximumConversations) _chats.Remove(_chats.Where(pair => pair.Key != currentId).MinBy(pair => pair.Value.Touched).Key);
        var keys = new HashSet<string>(StringComparer.Ordinal);
        var next = items.Where(item => item is not null && !string.IsNullOrWhiteSpace(item.Key) && item.Key.Length <= 512 &&
            !item.Key.Any(char.IsControl) && item.Kind is "screenshot" or "terminal" && item.Title is not null && item.Title.Length <= 256 && keys.Add(item.Key)).ToArray();
        var previousKeys = tabs.Items.Select(item => item.Key).ToHashSet(StringComparer.Ordinal);
        var added = next.Where(item => !previousKeys.Contains(item.Key) && !tabs.Closed.ContainsKey(item.Key)).ToArray();
        tabs.Items = next;
        // A late/empty UI projection does not prove that a formal resource was deleted.
        // Remember recent absent closed keys so rehydration cannot silently reopen them.
        var absentClosed = tabs.Closed.Where(pair => !keys.Contains(pair.Key)).OrderByDescending(pair => pair.Value).ToArray();
        foreach (var pair in absentClosed.Skip(MaximumAbsentClosedTabs)) tabs.Closed.Remove(pair.Key);
        if (!autoOpenNew) foreach (var item in added) tabs.Closed[item.Key] = ++_version;
        // New resources open in the background. A stable, still-open selection never changes.
        if (keys.Count > 0 && (tabs.SelectedKey is null || !keys.Contains(tabs.SelectedKey) || tabs.Closed.ContainsKey(tabs.SelectedKey)))
            tabs.SelectedKey = tabs.Items.FirstOrDefault(item => !tabs.Closed.ContainsKey(item.Key))?.Key;
        return added.Length > 0;
    }

    public bool Select(string key)
    {
        if (Current is not { } tabs || tabs.Closed.ContainsKey(key) || !tabs.Items.Any(item => item.Key == key)) return false;
        tabs.SelectedKey = key; return true;
    }

    public bool Close(string key)
    {
        if (Current is not { } tabs || !tabs.Items.Any(item => item.Key == key) || tabs.Closed.ContainsKey(key)) return false;
        tabs.Closed.Add(key, ++_version);
        if (tabs.SelectedKey == key)
        {
            int index = Array.FindIndex(tabs.Items, item => item.Key == key);
            tabs.SelectedKey = tabs.Items.Skip(index + 1).FirstOrDefault(item => !tabs.Closed.ContainsKey(item.Key))?.Key ??
                tabs.Items.Take(index).LastOrDefault(item => !tabs.Closed.ContainsKey(item.Key))?.Key;
        }
        return true;
    }

    public bool CloseSelected() => SelectedTab is { } item && Close(item.Key);

    public bool Reopen(string key)
    {
        if (Current is not { } tabs || !tabs.Items.Any(item => item.Key == key)) return false;
        tabs.Closed.Remove(key); tabs.SelectedKey = key; return true;
    }

    public void Clear() { _chats.Clear(); ConversationId = null; }
}
