namespace KYNXA_Desktop.Services;

/// <summary>
/// Retain preview drafts within a shared memory budget, including keys and string storage.
/// 按共享内存预算保留预览状态，计入键和字符串占用。
/// </summary>
internal sealed class WorkspacePreviewStateCache(
    long capacityBytes = 8 * 1024 * 1024, int maximumEntries = 128, long maximumEntryBytes = 2 * 1024 * 1024)
{
    private sealed record Entry(string Value, long Bytes, LinkedListNode<string> Position);
    private readonly Dictionary<string, Entry> _entries = new(StringComparer.Ordinal);
    private readonly LinkedList<string> _order = new();
    public long RetainedBytes { get; private set; }
    public int Count => _entries.Count;
    public IEnumerable<string> Keys => _entries.Keys;

    public bool TryGetValue(string key, out string value)
    {
        if (!_entries.TryGetValue(key, out var entry)) { value = null!; return false; }
        _order.Remove(entry.Position); _order.AddLast(entry.Position);
        value = entry.Value;
        return true;
    }

    public string? GetValueOrDefault(string key) => TryGetValue(key, out var value) ? value : null;
    public string GetValueOrDefault(string key, string fallback) => GetValueOrDefault(key) ?? fallback;

    public IReadOnlyList<string> Store(string key, string state)
    {
        long bytes = ((long)key.Length + state.Length) * sizeof(char) + 128;
        if (bytes > maximumEntryBytes || bytes > capacityBytes || maximumEntries < 1) return [];
        Remove(key);
        var position = _order.AddLast(key);
        _entries[key] = new(state, bytes, position);
        RetainedBytes += bytes;
        List<string> evicted = [];
        while (_entries.Count > maximumEntries || RetainedBytes > capacityBytes)
        {
            string oldest = _order.First!.Value;
            Remove(oldest); evicted.Add(oldest);
        }
        return evicted;
    }

    public bool Remove(string key)
    {
        if (!_entries.Remove(key, out var entry)) return false;
        _order.Remove(entry.Position); RetainedBytes -= entry.Bytes;
        return true;
    }

    public void Clear() { _entries.Clear(); _order.Clear(); RetainedBytes = 0; }
}
