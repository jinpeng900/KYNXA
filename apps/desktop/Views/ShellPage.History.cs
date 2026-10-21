using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.Controls;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private sealed class HistoryState(ProjectChatState chat)
    {
        public ProjectChatState Chat { get; } = chat;
        public bool Initialized { get; set; }
        public bool Loading { get; set; }
        public bool HasMore { get; set; }
        public string? Cursor { get; set; }
        public ConversationHistoryWindow Window { get; } = new();
        public long LastAccess { get; set; }
        public long CachedCharacters { get; set; }
        public long ProjectionVersion { get; set; }
        public ConversationHistoryPage? LatestPage { get; set; }
        public long LatestPageCharacters { get; set; }
        public bool LatestPagePending { get; set; }
    }

    private readonly Dictionary<Guid, HistoryState> _historyPages = [];
    private readonly CancellationTokenSource _historyLifetime = new();
    private long _historyAccessSequence;
    private Guid? _historyConversationId;
    private const int InitialHistoryPageMessages = 4;
    private const int InitialHistoryPageBytes = 256 * 1024;
    private const int RecentHistoryPageMessages = 40;
    private const int RecentHistoryPageBytes = 1024 * 1024;
    private const long MaximumInactiveHistoryCharacters = 32 * 1024 * 1024;
    private const int MaximumInactiveHistoryChats = 32;

    private Task EnsureRecentHistoryAsync(ProjectChatState chat)
    {
        if (!chat.IsPersisted || _projectViewClosed) return Task.CompletedTask;
        bool opened = _historyConversationId != chat.Id;
        _historyConversationId = chat.Id;
        if (!_historyPages.TryGetValue(chat.Id, out var state) || state.Chat != chat)
        {
            // Legacy gateways and test fixtures may still provide a complete catalog body.
            // 旧网关与测试夹具可能仍返回完整正文，保持兼容，不再次重复加载。
            state = new(chat) { Initialized = chat.Messages.Count > 0 };
            _historyPages[chat.Id] = state;
            if (state.Initialized) _ = MeasureHistoryProjectionAsync(state);
        }
        if (opened && state.Initialized && (state.Window.HasNewer || state.LatestPagePending) && state.LatestPage is { } latest)
        {
            // Returning from old history opens the retained latest page first; no old-page flash before network refresh.
            // 从旧历史切回聊天先使用保留的最近页，不先闪出旧页再等待网络刷新。
            chat.Messages = state.Window.Apply(latest, state.LatestPageCharacters, newer: false, reset: true, PreserveLocalHistory(chat));
            _projectStore.AcceptHistoryPage(latest);
            state.HasMore = state.Window.HasOlder; state.Cursor = state.Window.OlderCursor;
            state.ProjectionVersion++;
            state.CachedCharacters = state.Window.Characters;
            state.LatestPagePending = false;
        }
        state.LastAccess = ++_historyAccessSequence;
        TrimInactiveHistory();
        if (state.Initialized) PublishHistoryAvailability(state);
        if (state.Loading) return Task.CompletedTask;
        return !state.Initialized ? LoadHistoryAsync(state, "initial")
            : opened ? LoadHistoryAsync(state, "refresh") : Task.CompletedTask;
    }

    private async void Transcript_HistoryRequested(object? sender, Guid conversationId)
    {
        if (_projectViewClosed || _chatClosing || ActiveChatId != conversationId
            || !_historyPages.TryGetValue(conversationId, out var state)
            || !state.Initialized || !state.HasMore || state.Loading) return;
        await LoadHistoryAsync(state, "older");
    }

    private async void Transcript_HistoryNavigationRequested(object? sender, ConversationHistoryRequest request)
    {
        if (_projectViewClosed || _chatClosing || ActiveChatId != request.ConversationId
            || !_historyPages.TryGetValue(request.ConversationId, out var state) || state.Loading) return;
        if (request.Direction == "newer" && !state.Window.HasNewer)
        {
            if (state.LatestPagePending) await LoadHistoryAsync(state, "latest");
            return;
        }
        await LoadHistoryAsync(state, request.Direction);
    }

    private async Task LoadHistoryAsync(HistoryState state, string direction)
    {
        state.Loading = true;
        try
        {
            var chat = state.Chat;
            ConversationHistoryPage page;
            bool older = direction == "older", newer = direction == "newer";
            bool reset = !older && !newer;
            bool initial = direction == "initial";
            try
            {
                page = await _projectStore.LoadHistoryPageAsync(chat.Id, older ? state.Window.OlderCursor : newer ? state.Window.NewerCursor : null,
                    limit: initial ? InitialHistoryPageMessages : reset ? RecentHistoryPageMessages : 200, cancellationToken: _historyLifetime.Token,
                    maximumBytes: initial ? InitialHistoryPageBytes : reset ? RecentHistoryPageBytes : null);
            }
            catch (GatewayApiException error) when (!reset && error.ErrorCode == "CONVERSATION_HISTORY_CHANGED")
            {
                // A changed snapshot restarts at the latest page; old pages cannot mix into the new version.
                // 快照变化后重读最近页，旧页不能混入新版本，同时保留本机正在发送的消息。
                page = await _projectStore.LoadHistoryPageAsync(chat.Id, limit: RecentHistoryPageMessages,
                    cancellationToken: _historyLifetime.Token, maximumBytes: RecentHistoryPageBytes);
                reset = true;
            }
            if (_projectViewClosed || _historyLifetime.IsCancellationRequested
                || !_historyPages.TryGetValue(chat.Id, out var activeState) || activeState != state) return;
            if (direction == "refresh" && page.Snapshot is not null && page.Snapshot == state.LatestPage?.Snapshot
                && page.StartIndex == state.LatestPage.StartIndex && page.EndIndex == state.LatestPage.EndIndex) return;
            var preserved = PreserveLocalHistory(chat);
            // Count original text and tool payloads away from the UI thread before applying this page.
            // 在 UI 线程之外统计原文及工具回执，再应用有界页面窗口。
            var measured = await Task.Run(() => (Page: ConversationHistoryWindow.Measure(page.Messages),
                Preserved: ConversationHistoryWindow.Measure(preserved)));
            if (_projectViewClosed || _historyLifetime.IsCancellationRequested) return;
            if (direction == "refresh" && ActiveChatId == chat.Id && !await ConversationMessages.IsFollowingLatestAsync(chat.Id))
            {
                // A background refresh must not replace the page being read or selected; keep it ready for explicit latest/reopen.
                // 后台刷新不替换正在阅读或选择的页面；保留新最近页，供明确跳转或再次打开时使用。
                state.LatestPage = page; state.LatestPageCharacters = measured.Page; state.LatestPagePending = true;
                state.CachedCharacters = state.Window.Characters + measured.Page + measured.Preserved;
                PublishHistoryAvailability(state);
                return;
            }
            // New sends can arrive during background measurement; capture the live tail again before replacement.
            // 后台统计期间仍可能发送新消息，替换历史窗口前再次保留即时尾部，不能丢掉刚发送的原文。
            preserved = PreserveLocalHistory(chat);
            chat.Messages = state.Window.Apply(page, measured.Page, newer, reset, preserved);
            if (!page.HasNewer) { state.LatestPage = page; state.LatestPageCharacters = measured.Page; }
            state.LatestPagePending = false;
            state.ProjectionVersion++;
            state.CachedCharacters = state.Window.Characters + measured.Preserved;
            _projectStore.RetainHistoryProjection(chat.Id, chat.Messages.Select(message => message.Id).ToHashSet());
            state.Initialized = true;
            state.HasMore = state.Window.HasOlder;
            state.Cursor = state.Window.OlderCursor;
            state.LastAccess = ++_historyAccessSequence;
            if (ActiveChatId == chat.Id) UpdateConversationPresentation(openAtBottom: direction == "latest");
            PublishHistoryAvailability(state);
            TrimInactiveHistory();
            _ = MeasureHistoryProjectionAsync(state);
        }
        catch (OperationCanceledException) when (_historyLifetime.IsCancellationRequested) { }
        catch (Exception error)
        {
            // Do not erase cached messages or send another model request when history loading fails.
            // 历史读取失败保留已有消息，不清空对话，也不另派发模型请求。
            if (!_projectViewClosed && !_chatClosing)
                await ShowProjectErrorAsync(UiText.Get("项目操作未完成"), error.Message);
        }
        finally { state.Loading = false; }
        // Display the small recent tail before fetching the ordinary page; reuse refresh anchoring for readers who scroll up.
        // 最近尾部显示后再后台补齐普通页；用户已上翻时沿用刷新锚点保护，不替换正在阅读的内容。
        if (direction == "initial" && state.Initialized && state.HasMore && !_projectViewClosed
            && ActiveChatId == state.Chat.Id && !_historyLifetime.IsCancellationRequested)
            _ = LoadHistoryAsync(state, "refresh");
    }

    private List<ChatMessageState> PreserveLocalHistory(ProjectChatState chat)
    {
        var preserved = _projectStore.UnpersistedHistoryTail(chat);
        if (_pendingReplies.TryGetValue(chat.Id, out var running))
            preserved.AddRange(chat.Messages.Where(message => message == running.Message || message.Id == running.UserMessageId));
        return preserved;
    }

    private void PublishHistoryAvailability(HistoryState state)
    {
        if (ActiveChatId == state.Chat.Id)
            ConversationMessages.SetHistoryAvailability(state.Chat.Id, state.Window.HasOlder,
                state.Window.HasNewer || state.LatestPagePending, hasNewReplies: state.LatestPagePending);
    }

    private void TrimInactiveHistory()
    {
        var inactive = _historyPages.Values.Where(state => state.Initialized && state.Chat.Id != ActiveChatId).ToList();
        long characters = inactive.Sum(state => state.CachedCharacters);
        int count = inactive.Count;
        foreach (var state in inactive.OrderBy(state => state.LastAccess))
        {
            if (characters <= MaximumInactiveHistoryCharacters && count <= MaximumInactiveHistoryChats) break;
            if (state.Loading || _pendingReplies.ContainsKey(state.Chat.Id) || !_projectStore.CanReleaseHistory(state.Chat)) continue;
            characters -= state.CachedCharacters; count--;
            var ids = state.Chat.Messages.Concat(state.LatestPage?.Messages ?? []).Select(message => message.Id).Distinct().ToArray();
            state.Chat.Messages.Clear(); state.Window.Clear(); state.Initialized = false; state.HasMore = false; state.Cursor = null;
            state.LatestPage = null; state.LatestPageCharacters = 0; state.LatestPagePending = false;
            state.ProjectionVersion++;
            state.CachedCharacters = 0;
            _projectStore.ReleaseHistoryProjection(state.Chat.Id);
            ConversationMessages.ForgetConversation(state.Chat.Id, ids);
        }
    }

    private async Task MeasureHistoryProjectionAsync(HistoryState state)
    {
        long version = state.ProjectionVersion;
        var snapshot = state.Chat.Messages.ToArray();
        long characters = await Task.Run(() => ConversationHistoryWindow.Measure(snapshot));
        if (_projectViewClosed || state.ProjectionVersion != version
            || !_historyPages.TryGetValue(state.Chat.Id, out var current) || current != state) return;
        state.CachedCharacters = characters + (state.Window.HasNewer || state.LatestPagePending ? state.LatestPageCharacters : 0);
        // Compact newly accumulated replies, not an oversized page that the gateway deliberately retained.
        // 只收缩新累积的回复，不反复重读网关为保留原文而返回的单个超额页。
        if (!_pendingReplies.ContainsKey(state.Chat.Id) && !state.Loading && _projectStore.CanReleaseHistory(state.Chat)
            && snapshot.Length > state.Window.MessageCount
            && (snapshot.Length > 2000 || characters > 16 * 1024 * 1024))
            await LoadHistoryAsync(state, "latest");
        TrimInactiveHistory();
    }

    private async Task MaintainHistoryAfterReplyAsync(Guid chatId)
    {
        var (chat, _) = FindChat(chatId);
        if (chat is null || !chat.IsPersisted || _projectViewClosed || _pendingReplies.ContainsKey(chatId)) return;
        if (!_historyPages.TryGetValue(chatId, out var state))
            _historyPages[chatId] = state = new(chat) { Initialized = true };
        await MeasureHistoryProjectionAsync(state);
    }

    private void StopHistoryLoads()
    {
        _historyLifetime.Cancel();
        _historyPages.Clear();
        _historyLifetime.Dispose();
    }
}
