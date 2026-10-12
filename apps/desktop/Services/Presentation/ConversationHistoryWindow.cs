using KYNXA_Desktop.Models.UI;

namespace KYNXA_Desktop.Services;

/// <summary>
/// A replaceable display window over durable history, with cursors on both retained edges.
/// 正式聊天历史的可替换展示窗口；保留两端游标，不在界面重建正式消息日志。
/// </summary>
internal sealed class ConversationHistoryWindow(int maximumMessages = 2000, long maximumCharacters = 16 * 1024 * 1024)
{
    private readonly LinkedList<(ConversationHistoryPage Page, long Characters)> _pages = [];
    public long Characters { get; private set; }
    public int MessageCount { get; private set; }
    public string? OlderCursor => _pages.First?.Value.Page.NextCursor;
    public string? NewerCursor => _pages.Last?.Value.Page.NewerCursor;
    public bool HasOlder => _pages.First?.Value.Page.HasMore == true;
    public bool HasNewer => _pages.Last?.Value.Page.HasNewer == true;
    public bool ExceedsBudget => MessageCount > maximumMessages || Characters > maximumCharacters;

    public static long Measure(IReadOnlyList<ChatMessageState> messages) => messages.Sum(message =>
        (long)message.Content.Length + message.Reasoning.Length + message.AssistantSegments.Sum(segment =>
            (long)segment.Content.Length + segment.Reasoning.Length) + message.ToolActivities.Sum(tool =>
            (long)tool.Summary.Length + (tool.Result?.Length ?? 0) + (tool.Arguments?.GetRawText().Length ?? 0)));

    public List<ChatMessageState> Apply(ConversationHistoryPage page, long characters, bool newer, bool reset,
        IReadOnlyList<ChatMessageState> preserved)
    {
        if (reset) Clear();
        if (newer) _pages.AddLast((page, characters)); else _pages.AddFirst((page, characters));
        Characters += characters; MessageCount += page.Messages.Count;
        // Keep the requested page and its adjacent page so the visible scroll anchor can survive.
        // An individually oversized page is reported rather than silently dropping its source.
        // 保留当前请求页及相邻页，让可见滚动锚点继续有效；单页超额明确记录，不静默丢弃原文。
        while (_pages.Count > 2 && ExceedsBudget)
        {
            var evicted = newer ? _pages.First! : _pages.Last!;
            Characters -= evicted.Value.Characters; MessageCount -= evicted.Value.Page.Messages.Count;
            _pages.Remove(evicted);
        }
        var output = new List<ChatMessageState>();
        var ids = new HashSet<Guid>();
        foreach (var message in _pages.SelectMany(entry => entry.Page.Messages))
            if (ids.Add(message.Id)) output.Add(message);
        foreach (var message in preserved)
        {
            int index = output.FindIndex(item => item.Id == message.Id);
            if (index >= 0) output[index] = message;
            else if (ids.Add(message.Id)) output.Add(message);
        }
        return output;
    }

    public void Clear() { _pages.Clear(); Characters = 0; MessageCount = 0; }
}
