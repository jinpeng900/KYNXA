namespace KYNXA_Desktop.Services;

public enum WorkspacePresentationPriority { CatalogOnly, Automatic, Requested, Final }

/// <summary>
/// Key is a logical call identity, not a mutable result-reference or output version.
/// 键使用逻辑调用身份，不使用可能变化的结果引用或输出版本。
/// </summary>
public sealed record ConversationWorkTab(string Key, string Kind, string Title,
    string? Target = null, string? Version = null, bool UserSelected = false,
    WorkspacePresentationPriority PresentationPriority = WorkspacePresentationPriority.Automatic, Guid? MountId = null);

/// <summary>
/// Per-conversation display state. Closing never deletes or cancels the source resource.
/// 按聊天隔离的展示状态；关闭标签不会删除资源或取消执行。
/// </summary>
public sealed class ConversationWorkTabState
{
    public const int MaximumConversations = 16;
    public const int MaximumAbsentClosedTabs = 256;
    private sealed class Tabs
    {
        public ConversationWorkTab[] Items { get; set; } = [];
        public Dictionary<string, long> Closed { get; } = new(StringComparer.Ordinal);
        public Dictionary<string, long> ExplicitlyOpened { get; } = new(StringComparer.Ordinal);
        public HashSet<string> AutomaticallyHidden { get; } = new(StringComparer.Ordinal);
        public string? SelectedKey { get; set; }
        public long Touched { get; set; }
        public bool HasCustomOrder { get; set; }
        public bool HomeSelected { get; set; }
    }

    private readonly Dictionary<Guid, Tabs> _chats = [];
    private long _version;
    private Tabs? Current => ConversationId is Guid id && _chats.TryGetValue(id, out var tabs) ? tabs : null;
    public Guid? ConversationId { get; private set; }
    public IReadOnlyList<ConversationWorkTab> Items => Current?.Items ?? [];
    public IReadOnlyList<ConversationWorkTab> OpenItems => Current is { } tabs ? tabs.Items.Where(item => IsOpen(tabs, item.Key)).ToArray() : [];
    public ConversationWorkTab? SelectedTab => Current is { } tabs ? tabs.Items.FirstOrDefault(item => item.Key == tabs.SelectedKey && IsOpen(tabs, item.Key)) : null;
    public bool HasItems => Items.Count > 0;
    public bool HasOpenTabs => OpenItems.Count > 0;

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
            !item.Key.Any(char.IsControl) && item.Kind is "screenshot" or "terminal" or "document" or "diagram" or "browser" or "changes" &&
            item.Title is not null && item.Title.Length <= 256 && keys.Add(item.Key)).ToArray();
        if (tabs.HasCustomOrder)
        {
            var order = tabs.Items.Select((item, index) => (item.Key, index)).ToDictionary(item => item.Key, item => item.index, StringComparer.Ordinal);
            next = next.OrderBy(item => order.GetValueOrDefault(item.Key, int.MaxValue)).ToArray();
        }
        var previousKeys = tabs.Items.Select(item => item.Key).ToHashSet(StringComparer.Ordinal);
        var added = next.Where(item => !previousKeys.Contains(item.Key) && !tabs.Closed.ContainsKey(item.Key)).ToArray();
        foreach (var item in next.Where(item => item.UserSelected)) tabs.ExplicitlyOpened[item.Key] = ++_version;
        tabs.Items = next;
        // A late/empty UI projection does not prove that a formal resource was deleted.
        // Remember recent absent closed keys so rehydration cannot silently reopen them.
        // 晚到或空的 UI 投影不证明正式资源已删除；保留近期关闭键，避免重新加载后悄悄打开。
        var absentClosed = tabs.Closed.Where(pair => !keys.Contains(pair.Key)).OrderByDescending(pair => pair.Value).ToArray();
        foreach (var pair in absentClosed.Skip(MaximumAbsentClosedTabs)) tabs.Closed.Remove(pair.Key);
        foreach (var pair in tabs.ExplicitlyOpened.Where(pair => !keys.Contains(pair.Key)).OrderByDescending(pair => pair.Value).Skip(MaximumAbsentClosedTabs).ToArray())
            tabs.ExplicitlyOpened.Remove(pair.Key);
        if (!autoOpenNew) foreach (var item in added) tabs.Closed[item.Key] = ++_version;
        ApplyAutomaticPresentation(tabs);
        // Background presentation preserves the current reader; lower-priority files remain reopenable.
        // 后台展示保留正在阅读的标签；低优先级文件仍在目录中，用户可按需打开。
        if (!tabs.HomeSelected && keys.Count > 0 && (tabs.SelectedKey is null || !keys.Contains(tabs.SelectedKey) || tabs.Closed.ContainsKey(tabs.SelectedKey)))
            tabs.SelectedKey = tabs.Items.Where(item => IsOpen(tabs, item.Key)).OrderByDescending(item => item.PresentationPriority).FirstOrDefault()?.Key;
        return added.Length > 0;
    }

    private static bool IsOpen(Tabs tabs, string key) => !tabs.Closed.ContainsKey(key) && !tabs.AutomaticallyHidden.Contains(key);

    private static void ApplyAutomaticPresentation(Tabs tabs)
    {
        // Choose by purpose, never a fixed count; requested/final outputs stay available together.
        // 按用途选择，不按固定数量截断；明确展示与最终成果可以同时保留。
        var selected = tabs.Items.FirstOrDefault(item => item.Key == tabs.SelectedKey && IsOpen(tabs, item.Key));
        tabs.AutomaticallyHidden.Clear();
        foreach (var item in tabs.Items.Where(item => item.PresentationPriority == WorkspacePresentationPriority.CatalogOnly &&
            item.Key != selected?.Key && !tabs.ExplicitlyOpened.ContainsKey(item.Key)))
            tabs.AutomaticallyHidden.Add(item.Key);
    }

    public bool Select(string key)
    {
        if (Current is not { } tabs || !IsOpen(tabs, key) || !tabs.Items.Any(item => item.Key == key)) return false;
        tabs.ExplicitlyOpened[key] = ++_version;
        tabs.HomeSelected = false; tabs.SelectedKey = key; return true;
    }

    public bool Close(string key)
    {
        if (Current is not { } tabs || !tabs.Items.Any(item => item.Key == key) || !IsOpen(tabs, key)) return false;
        tabs.Closed.Add(key, ++_version);
        if (tabs.SelectedKey == key)
        {
            int index = Array.FindIndex(tabs.Items, item => item.Key == key);
            tabs.SelectedKey = tabs.Items.Skip(index + 1).FirstOrDefault(item => IsOpen(tabs, item.Key))?.Key ??
                tabs.Items.Take(index).LastOrDefault(item => IsOpen(tabs, item.Key))?.Key;
        }
        return true;
    }

    public bool CloseSelected() => SelectedTab is { } item && Close(item.Key);

    public bool Move(string key, string beforeKey)
    {
        if (Current is not { } tabs || key == beforeKey) return false;
        var items = tabs.Items.ToList();
        var item = items.FirstOrDefault(item => item.Key == key);
        if (item is null || !items.Any(item => item.Key == beforeKey)) return false;
        items.Remove(item); items.Insert(items.FindIndex(item => item.Key == beforeKey), item);
        tabs.Items = items.ToArray(); tabs.HasCustomOrder = true;
        return true;
    }

    public bool Reopen(string key)
    {
        if (Current is not { } tabs || !tabs.Items.Any(item => item.Key == key)) return false;
        tabs.HomeSelected = false; tabs.Closed.Remove(key); tabs.AutomaticallyHidden.Remove(key);
        tabs.ExplicitlyOpened[key] = ++_version; tabs.SelectedKey = key; return true;
    }

    public bool SelectHome()
    {
        if (Current is not { } tabs) return false;
        tabs.HomeSelected = true; tabs.SelectedKey = null; return true;
    }

    public void Clear() { _chats.Clear(); ConversationId = null; }
}
