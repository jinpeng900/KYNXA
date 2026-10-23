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

    private bool OwnsHistoryState(HistoryState state) => !_projectViewClosed && !_chatClosing
        && !_historyLifetime.IsCancellationRequested && _historyPages.TryGetValue(state.Chat.Id, out var current)
        && ReferenceEquals(current, state) && ReferenceEquals(FindChat(state.Chat.Id).Chat, state.Chat);

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
        if (!OwnsHistoryState(state) || state.Loading) return;
        state.Loading = true;
        var requestIds = state.Chat.Messages.Select(message => message.Id).ToHashSet();
        var liveAtRequest = PreserveLocalHistory(state.Chat).DistinctBy(message => message.Id).ToArray();
        List<ChatMessageState> PreserveRequestTail() => PreserveLocalHistory(state.Chat)
            .Concat(liveAtRequest).Concat(state.Chat.Messages.Where(message => !requestIds.Contains(message.Id)))
            .DistinctBy(message => message.Id).ToList();
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
                if (!OwnsHistoryState(state)) return;
                // A changed snapshot restarts at the latest page; old pages cannot mix into the new version.
                // 快照变化后重读最近页，旧页不能混入新版本，同时保留本机正在发送的消息。
                page = await _projectStore.LoadHistoryPageAsync(chat.Id, limit: RecentHistoryPageMessages,
                    cancellationToken: _historyLifetime.Token, maximumBytes: RecentHistoryPageBytes);
                reset = true;
            }
            if (!OwnsHistoryState(state)) return;
            if (direction == "refresh" && liveAtRequest.Length == 0 && page.Snapshot is not null && page.Snapshot == state.LatestPage?.Snapshot
                && page.StartIndex == state.LatestPage.StartIndex && page.EndIndex == state.LatestPage.EndIndex) return;
            var preserved = PreserveRequestTail();
            // Count original text and tool payloads away from the UI thread before applying this page.
            // 在 UI 线程之外统计原文及工具回执，再应用有界页面窗口。
            var measured = await Task.Run(() => (Page: ConversationHistoryWindow.Measure(page.Messages),
                Preserved: ConversationHistoryWindow.Measure(preserved)));
            if (!OwnsHistoryState(state)) return;
            bool followingLatest = direction != "refresh" || ActiveChatId != chat.Id
                || await ConversationMessages.IsFollowingLatestAsync(chat.Id);
            if (!OwnsHistoryState(state)) return;
            preserved = PreserveRequestTail();
            bool hasNewerLocalTail = preserved.Count > 0;
            if (!followingLatest)
            {
                // Keep refreshed history ready for latest/reopen without replacing the reader's page.
                // 后台新页留待跳转或重开，不替换正在阅读或选择的页面。
                state.LatestPage = hasNewerLocalTail ? null : page;
                state.LatestPageCharacters = measured.Page; state.LatestPagePending = true;
                state.CachedCharacters = state.Window.Characters + measured.Page + measured.Preserved;
                PublishHistoryAvailability(state);
                return;
            }
            // Re-capture the live tail before replacement to preserve messages sent during measurement.
            // 替换历史窗口前重取实时尾部，保留后台统计期间新发的消息。
            chat.Messages = state.Window.Apply(page, measured.Page, newer, reset, preserved);
            // A page predating live output is safe to merge, but unsafe to reuse later as the latest snapshot.
            // 早于实时输出的分页可合并，却不能以后当最近快照再倒写；保留原分页与本机尾部各自身份。
            if (hasNewerLocalTail) state.LatestPage = null;
            else if (!page.HasNewer) { state.LatestPage = page; state.LatestPageCharacters = measured.Page; }
            state.LatestPagePending = false;
            state.ProjectionVersion++;
            state.CachedCharacters = state.Window.Characters + measured.Preserved
                + (state.Window.HasNewer ? state.LatestPageCharacters : 0);
            _projectStore.RetainHistoryProjection(chat.Id, chat.Messages.Select(message => message.Id).ToHashSet());
            state.Initialized = true;
            state.HasMore = state.Window.HasOlder;
            state.Cursor = state.Window.OlderCursor;
            state.LastAccess = ++_historyAccessSequence;
            if (ActiveChatId == chat.Id) UpdateConversationPresentation(openAtBottom: direction == "latest");
            PublishHistoryAvailability(state);
            TrimInactiveHistory();
            // Page totals are already measured; only a live local tail can add work outside the page window.
            // 分页总量已经统计，只有本机实时尾部可能在页面窗口之外新增内容。
            if (preserved.Count > 0) _ = MeasureHistoryProjectionAsync(state);
        }
        catch (OperationCanceledException) when (_historyLifetime.IsCancellationRequested) { }
        catch (Exception error)
        {
            // Do not erase cached messages or send another model request when history loading fails.
            // 历史读取失败保留已有消息，不清空对话，也不另派发模型请求。
            if (OwnsHistoryState(state) && ActiveChatId == state.Chat.Id)
                ShowActionFeedback(UiText.Get("项目操作未完成") + ": " + error.Message);
        }
        finally
        {
            state.Loading = false;
            if (OwnsHistoryState(state) && ActiveChatId == state.Chat.Id)
                RecoverRunningTaskObservation(state.Chat, FindChat(state.Chat.Id).Project?.Id);
        }
        // Show the recent tail first; fetch the full page without disturbing readers who scroll up.
        // 先显示最近尾部，再后台补页；用户上翻时保留阅读锚点和内容。
        if (direction == "initial" && state.Initialized && state.HasMore && OwnsHistoryState(state)
            && ActiveChatId == state.Chat.Id)
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
        if (!OwnsHistoryState(state) || state.ProjectionVersion != version) return;
        state.CachedCharacters = characters + (state.Window.HasNewer || state.LatestPagePending ? state.LatestPageCharacters : 0);
        // Compact newly accumulated replies, not an oversized page that the gateway deliberately retained.
        // 只收缩新累积的回复，不反复重读网关为保留原文而返回的单个超额页。
        if (!_pendingReplies.ContainsKey(state.Chat.Id) && !state.Loading && _projectStore.CanReleaseHistory(state.Chat)
            && snapshot.Length > state.Window.MessageCount
            && (snapshot.Length > 2000 || characters > 16 * 1024 * 1024))
            await LoadHistoryAsync(state, "latest");
        if (!OwnsHistoryState(state)) return;
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
