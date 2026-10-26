using KYNXA_Desktop.Services;
using System.Diagnostics;
using System.Text;
using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private sealed class PendingChatReply(Guid conversationId, Guid userMessageId, string question, string? provider, string? model,
        string permissionMode, ChatMessageState? restoredMessage = null)
    {
        public Guid ConversationId { get; } = conversationId;
        public Guid UserMessageId { get; } = userMessageId;
        public string Question { get; } = question;
        public string? Provider { get; } = provider;
        public string? Model { get; } = model;
        public string PermissionMode { get; } = permissionMode;
        public CancellationTokenSource Cancellation { get; } = new();
        public ChatMessageState Message { get; set; } = restoredMessage ?? new() { Role = "assistant", Status = "streaming", Provider = provider ?? "", Model = model ?? "" };
        public Guid StreamRequestId { get; set; }
        public Guid? WaitingReplyAnchorId { get; set; }
        public ConversationMessageViewModel Presentation { get; set; } = null!;
        public StringBuilder Content { get; } = new();
        public StringBuilder Reasoning { get; } = new();
        public Dictionary<string, (StringBuilder Content, StringBuilder Reasoning)> SegmentText { get; } = new(StringComparer.Ordinal);
        public HashSet<string> DirtySegments { get; } = new(StringComparer.Ordinal);
        public Stopwatch ThinkingTime { get; } = new();
        public long LastTextFlushTimestamp { get; set; }
        public Guid LastTextFlushMessageId { get; set; }
        public bool Dirty { get; set; }
        public bool Started { get; set; }
        public bool UserPersisted { get; set; }
        public bool StopRequested { get; set; }
        public bool StopControlPending { get; set; }
        public int StopRegistrationRetries { get; set; }
        public Guid? TaskId { get; set; }
        public bool PauseConfirmed { get; set; }
        public TaskCompletionSource Finished { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public TaskCompletionSource<Guid?> TaskRegistered { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public string? Error => Message.Status == "streaming" ? null : Message.Error;
    }

    private readonly Dictionary<Guid, PendingChatReply> _pendingReplies = [];
    private DispatcherTimer? _replyRefreshTimer;
    private bool _chatClosing;
    private Guid? ActiveChatId => (ViewModel.IsChatMode ? _activeStandaloneChat : _activeProjectChat)?.Id;

    private bool IsReplyInProgress(Guid? chatId) => chatId is Guid id && _pendingReplies.ContainsKey(id);

    private void UpdateSendButtonState()
    {
        if (SendButton is null) return;
        RefreshUsageSummaryScope();
        bool active = ActiveChatId is Guid id && _pendingReplies.ContainsKey(id);
        bool queued = ActiveChatId is Guid queueChat && HasQueuedTaskActivity(queueChat);
        bool hasInput = !string.IsNullOrWhiteSpace(PromptTextBox.Text) || HasAttachmentDrafts;
        SendButton.IsEnabled = active || (!_sendingPrompt && _projectsReady && !_projectActionPending
            && hasInput);
        // Retain the shared theme brush while the durable-task action determines the visible icon.
        // 保留共享主题画刷；可见图标由持久任务的当前动作决定。
        SendArrow.Stroke = AppearanceService.GetBrush(SendButton.IsEnabled ? "KynxaOnAccentBrush" : "KynxaDisabledTextBrush");
        bool pause = active && !hasInput;
        SendArrow.Visibility = pause ? Visibility.Collapsed : Visibility.Visible;
        StopReplyIcon.Visibility = pause ? Visibility.Visible : Visibility.Collapsed;
        string caption = pause ? "暂停并保存进度" : queued ? "加入等待队列" : "发送";
        AutomationProperties.SetName(SendButton, UiText.Get(caption));
        ToolTipService.SetToolTip(SendButton, UiText.Get(caption));
    }

    private PendingChatReply BeginPendingReply(Guid chatId, string question, string? provider, string? model, string? permissionMode = null, Guid? requestId = null, Guid? userMessageId = null)
    {
        var (chat, _) = FindChat(chatId);
        if (chat is null || _pendingReplies.ContainsKey(chatId)) throw new InvalidOperationException(UiText.Get("当前聊天正在生成回复。"));
        var user = chat.Messages.LastOrDefault(message => message.Role == "user" && (userMessageId is null || message.Id == userMessageId))
            ?? throw new InvalidOperationException(UiText.Get("找不到要回复的用户消息。"));
        var pending = new PendingChatReply(chatId, user.Id, question, provider, model, permissionMode ?? _layout.PermissionMode);
        if (requestId is Guid existingId) pending.Message.Id = existingId;
        pending.StreamRequestId = pending.Message.Id;
        pending.Presentation = new ConversationMessageViewModel(chatId, pending.Message);
        pending.Presentation.SetRetryAllowed(true);
        chat.Messages.Add(pending.Message);
        _pendingReplies.Add(chatId, pending);
        RenderProjects();
        StartReplyRefreshTimer();
        return pending;
    }

    private void StartReplyRefreshTimer()
    {
        if (_replyRefreshTimer is null)
        {
            _replyRefreshTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(40) };
            _replyRefreshTimer.Tick += (_, _) =>
            {
                foreach (var reply in _pendingReplies.Values.ToArray()) FlushReply(reply);
            };
        }
        _replyRefreshTimer.Start();
    }

    private (ProjectChatState? Chat, ProjectState? Project) FindChat(Guid chatId)
    {
        var standalone = _standaloneChats.FirstOrDefault(chat => chat.Id == chatId);
        if (standalone is not null) return (standalone, null);
        foreach (var project in _projects)
        {
            var chat = project.Chats.FirstOrDefault(chat => chat.Id == chatId);
            if (chat is not null) return (chat, project);
        }
        return (null, null);
    }

    private void FlushReply(PendingChatReply pending, bool final = false)
    {
        if (!pending.Dirty && !final) return;
        bool textChanged = pending.Message.Content.Length != pending.Content.Length
            || pending.Message.Reasoning.Length != pending.Reasoning.Length;
        // Use a 120 ms snapshot interval above 256 KiB of growing text; anchors and final state flush immediately.
        // 增长中的文本超过 256 KiB 时按 120 毫秒合并快照；新回复锚和终态立即刷新。
        if (!final && textChanged && (long)pending.Content.Length + pending.Reasoning.Length >= 128 * 1024
            && pending.LastTextFlushMessageId == pending.Message.Id && pending.LastTextFlushTimestamp != 0
            && Stopwatch.GetElapsedTime(pending.LastTextFlushTimestamp).TotalMilliseconds < 120) return;
        foreach (string id in pending.DirtySegments)
        {
            int index = pending.Message.AssistantSegments.FindIndex(segment => segment.Id == id);
            if (index >= 0 && pending.SegmentText.TryGetValue(id, out var text))
            {
                var segment = pending.Message.AssistantSegments[index];
                pending.Message.AssistantSegments[index] = segment with
                {
                    Content = segment.Content.Length == text.Content.Length ? segment.Content : text.Content.ToString(),
                    Reasoning = segment.Reasoning.Length == text.Reasoning.Length ? segment.Reasoning : text.Reasoning.ToString()
                };
            }
        }
        pending.DirtySegments.Clear();
        // Absolute replacements update Message too; unchanged channels reuse their existing strings.
        // 绝对快照同步更新 Message；未增长的通道直接复用已有字符串。
        if (pending.Message.Content.Length != pending.Content.Length) pending.Message.Content = pending.Content.ToString();
        if (pending.Message.Reasoning.Length != pending.Reasoning.Length) pending.Message.Reasoning = pending.Reasoning.ToString();
        if (textChanged)
        {
            pending.LastTextFlushTimestamp = Stopwatch.GetTimestamp();
            pending.LastTextFlushMessageId = pending.Message.Id;
        }
        pending.Message.ReasoningDurationMs = pending.ThinkingTime.ElapsedMilliseconds;
        pending.Presentation.Refresh(pending.ThinkingTime.IsRunning);
        pending.Dirty = false;
    }

    private async Task ReceiveReplyAsync(PendingChatReply pending, Guid? observedTaskId = null)
    {
        bool isPrepared = observedTaskId is not null;
        bool isSubmitted = observedTaskId is not null;
        int projectionRecoveryAttempt = 0;
        try
        {
            while (true)
            {
                try
                {
                    // Storage and connection preparation follow the already visible user and waiting rows.
                    // 用户消息与等待占位已显示后再准备正式提交与连接；失败保留本轮稳定身份供重试。
                    if (!isPrepared)
                    {
                        await SavePendingUserMessageAsync(pending);
                        isPrepared = true;
                    }
                    if (pending.StopRequested) pending.Cancellation.Cancel();
                    pending.Cancellation.Token.ThrowIfCancellationRequested();
                    var updates = observedTaskId is Guid taskId
                        ? _modelApiClient.StreamTaskReplyAsync(pending.ConversationId, taskId, pending.StreamRequestId, pending.Cancellation.Token)
                        : isSubmitted ? _modelApiClient.ObserveRequestReplyAsync(pending.ConversationId, pending.StreamRequestId,
                            pending.TaskId, pending.Cancellation.Token)
                        : _modelApiClient.StreamReplyAsync(new ChatRequest(pending.ConversationId, pending.Question, pending.Model,
                            pending.PermissionMode, pending.Provider, pending.StreamRequestId, pending.UserMessageId,
                            FindChat(pending.ConversationId).Chat?.Messages.FirstOrDefault(message => message.Id == pending.UserMessageId)
                                ?.Attachments.Select(file => file.Id).ToArray()), pending.Cancellation.Token);
                    isSubmitted = true;
                    await foreach (var update in updates)
                    {
                        // Exit blocks new dispatches while the existing stream awaits its pause acknowledgement.
                        // 退出阻止新派发；已有观察保留到暂停受理，窗口关闭后不再处理界面事件。
                        if (_projectViewClosed || _chatClosing && !_savingOnClose ||
                            !_pendingReplies.TryGetValue(pending.ConversationId, out var current) || current != pending) return;
                        if (pending.WaitingReplyAnchorId is not null && update.Type is "reasoning_delta" or "text_delta"
                            or "assistant_segment" or "content_snapshot") continue;
                        if (pending.WaitingReplyAnchorId is Guid expectedReply && (update.Type is "completed" or "cancelled" or "failed")
                            && update.ReplyMessageId != expectedReply) continue;
                        // Transport acceptance can precede the durable write; model output confirms execution reached it.
                        // 传输接受可能早于正式写入；收到模型输出后才确认用户消息已进入持久化执行流程。
                        if (!pending.UserPersisted && update.Type is "reasoning_delta" or "text_delta" or "assistant_segment" or "completed")
                        {
                            _projectStore.AcceptSubmittedMessage(pending.ConversationId, pending.UserMessageId);
                            pending.UserPersisted = true;
                        }
                        // Timing is independent of visible prose; buffered tool-only work still starts the clock.
                        // 计时独立于可见正文；隐藏阶段文字的纯工具执行也必须启表，恢复沿用网关累计值。
                        if (update.Type != "reply_anchor" && update.GenerationElapsedMs is long elapsed && update.GenerationActive is bool generationActive)
                        {
                            pending.Message.ApplyGenerationTiming(elapsed, generationActive);
                            pending.Dirty = true;
                        }
                        switch (update.Type)
                        {
                            case "generation_timing":
                                break;
                            case "reply_anchor":
                                AcceptReplyAnchor(pending, update);
                                break;
                            case "started":
                                pending.Started = true;
                                pending.TaskId = update.RootRequestTaskId ?? update.TaskId ?? pending.TaskId;
                                if (pending.TaskId is Guid registered && _queuedPrompts.TryGetValue(pending.ConversationId, out var waiting))
                                    foreach (var guidance in waiting.Where(item => item.TargetRequestId == pending.StreamRequestId))
                                        guidance.TargetTaskId ??= registered;
                                pending.TaskRegistered.TrySetResult(pending.TaskId);
                                if (update.TaskId is not null) TaskPanel.NotifyChanged(pending.ConversationId);
                                if (pending.StopRequested) await StopPendingReplyAsync(pending);
                                break;
                            case "task_progress":
                                if (update.TaskId is null || update.TaskId == pending.TaskId)
                                {
                                    pending.Message.ExecutionPhase = update.Phase ?? string.Empty;
                                    pending.Presentation.Refresh(pending.ThinkingTime.IsRunning);
                                }
                                TaskPanel.NotifyChanged(pending.ConversationId);
                                continue;
                            case "task_tool_call":
                            case "task_tool_result":
                            case "task_finished":
                                TaskPanel.NotifyChanged(pending.ConversationId);
                                continue;
                            case "task_approval_required":
                                if (update.Tool is { } childApproval) QueueToolApproval(pending, childApproval, update.TaskId);
                                TaskPanel.NotifyChanged(pending.ConversationId);
                                continue;
                            case "task_approval_closed":
                                if (update.ApprovalId is Guid closedApproval) CloseTaskApproval(closedApproval, update.TaskId);
                                TaskPanel.NotifyChanged(pending.ConversationId);
                                continue;
                            case "approval_closed":
                                if (update.ApprovalId is Guid closedInlineApproval) CloseTaskApproval(closedInlineApproval, update.TaskId);
                                continue;
                            case "tool_call":
                            case "tool_result":
                                if (update.Tool is { } activity)
                                {
                                    if (pending.WaitingReplyAnchorId is null) AcceptToolActivity(pending, activity);
                                }
                                break;
                            case "terminal_output":
                                continue; // Command output stays in the formal tool receipt; it has no sidebar projection.
                                // 命令输出保留在正式工具回执中，不生成侧栏投影。
                            case "workspace_terminal_open":
                                AcceptWorkspaceTerminal(pending.ConversationId, update);
                                continue;
                            case "approval_required":
                                if (update.Tool is { } approval)
                                {
                                    AcceptToolActivity(pending, approval);
                                    FlushReply(pending);
                                    QueueToolApproval(pending, approval);
                                }
                                break;
                            case "reasoning_delta":
                                pending.Message.ExecutionPhase = string.Empty;
                                StartReplyTimer(pending);
                                pending.ThinkingTime.Start();
                                pending.Reasoning.Append(update.Delta);
                                AppendSegmentDelta(pending, update, reasoning: true);
                                break;
                            case "text_delta":
                                pending.Message.ExecutionPhase = string.Empty;
                                StartReplyTimer(pending);
                                pending.ThinkingTime.Stop();
                                pending.Content.Append(update.Delta);
                                AppendSegmentDelta(pending, update, reasoning: false);
                                break;
                            case "assistant_segment":
                                if (update.Segment is { } segment)
                                {
                                    if (segment.Status == "streaming") StartReplyTimer(pending);
                                    AcceptAssistantSegment(pending, segment);
                                }
                                break;
                            case "content_snapshot":
                                // Reattached streams publish absolute snapshots, including an intentionally empty current reply.
                                // 重附事件流给出绝对快照；当前回复为空也应覆盖旧投影，避免重复文字或遗留上一段内容。
                                AcceptReplyTextSnapshot(pending, update.Content, update.Reasoning);
                                break;
                            case "completed":
                            case "interrupted":
                            case "error":
                                pending.ThinkingTime.Stop();
                                if (!string.IsNullOrEmpty(update.Content) || pending.Content.Length == 0)
                                {
                                    pending.Message.Content = update.Content ?? string.Empty;
                                    pending.Content.Clear(); pending.Content.Append(pending.Message.Content);
                                }
                                if (update.Reasoning is not null)
                                {
                                    pending.Message.Reasoning = update.Reasoning;
                                    pending.Reasoning.Clear(); pending.Reasoning.Append(update.Reasoning);
                                }
                                if (update.AssistantSegments is { } segments && (segments.Any() || pending.Message.AssistantSegments.Count == 0))
                                {
                                    pending.Message.AssistantSegments.Clear();
                                    pending.Message.AssistantSegments.AddRange(segments);
                                    pending.SegmentText.Clear();
                                    pending.DirtySegments.Clear();
                                }
                                pending.Message.Status = update.Type == "completed" ? "completed" : update.Type;
                                pending.Message.ExecutionPhase = string.Empty;
                                SettleReplySegments(pending);
                                pending.Message.Error = update.Error ?? string.Empty;
                                pending.Message.DurationMs = update.DurationMs;
                                pending.Message.TaskCompletionState = ChatMessageState.ReadTaskCompletion(update.TaskCompletion);
                                if (update.Code == "TASK_PAUSED" && pending.TaskId is not null)
                                {
                                    pending.PauseConfirmed = true;
                                    pending.Message.Error = string.Empty;
                                }
                                MessageTimePresentation.RecordEnd(pending.ConversationId, pending.Message, DateTimeOffset.UtcNow);
                                FlushReply(pending, final: true);
                                break;
                        }
                        pending.Dirty = true;
                    }
                    if (pending.Message.Status == "streaming")
                    {
                        pending.Message.Status = "interrupted";
                        pending.Message.Error = UiText.Get("连接已结束，回复尚未完成。");
                    }
                    break;
                }
                catch (Exception error) when (pending.Started && !pending.Cancellation.IsCancellationRequested && !_chatClosing
                    && error is not GatewayApiException && error is (IOException or UnauthorizedAccessException or
                        InvalidOperationException or System.Text.Json.JsonException or System.Runtime.InteropServices.COMException))
                {
                    // Yield the UI thread, then observe the same request after a presentation failure.
                    // 展示失败不等于执行失败；让出界面线程后重接原请求，不重发消息或操作。
                    pending.Dirty = true;
                    observedTaskId = null;
                    int delayMs = projectionRecoveryAttempt++ switch { 0 => 100, 1 => 500, 2 => 1000, _ => 5000 };
                    await Task.Delay(delayMs, pending.Cancellation.Token);
                }
            }
        }
        catch (OperationCanceledException) when (pending.Cancellation.IsCancellationRequested || _chatClosing)
        {
            pending.Message.Status = "interrupted";
            pending.Message.Error = string.Empty;
        }
        catch (Exception error) when (error is System.Net.Http.HttpRequestException or OperationCanceledException or
            System.Text.Json.JsonException or InvalidDataException or IOException or UnauthorizedAccessException or InvalidOperationException)
        {
            pending.Message.Status = "error";
            pending.Message.Error = error is InvalidOperationException or InvalidDataException or GatewayApiException ? error.Message :
                UiText.Get("连接中断，已保留收到的内容。请检查模型服务后重试。");
        }
        finally
        {
            bool cachePresentation = false;
            try
            {
                CloseReplyApprovals(pending);
                pending.ThinkingTime.Stop();
                // Never leave a departed stream looking live, even if an unexpected exception escapes the handler.
                // 即使意外异常未被上面的处理器接住，已退出的流也不能继续显示为生成中。
                if (pending.Message.Status == "streaming")
                {
                    pending.Message.Status = "interrupted";
                    pending.Message.Error = UiText.Get("连接已结束，回复尚未完成。");
                }
                SettleReplySegments(pending);
                if (_pendingReplies.TryGetValue(pending.ConversationId, out var current) && current == pending)
                {
                    MessageTimePresentation.RecordEnd(pending.ConversationId, pending.Message, DateTimeOffset.UtcNow);
                    FlushReply(pending, final: true);
                    _pendingReplies.Remove(pending.ConversationId);
                    cachePresentation = true;
                }
            }
            finally
            {
                // Presentation-cache failure must not retain the running marker or cancellation resources.
                // 展示缓存失败也必须释放运行占位与取消资源，避免发送按钮永久停留在停止状态。
                if (_pendingReplies.TryGetValue(pending.ConversationId, out var current) && current == pending)
                    _pendingReplies.Remove(pending.ConversationId);
                pending.Cancellation.Dispose();
                pending.TaskRegistered.TrySetResult(null);
                pending.Finished.TrySetResult();
                if (_pendingReplies.Count == 0) _replyRefreshTimer?.Stop();
                if (!_chatClosing)
                {
                    ReleaseTaskReplyObservation(pending);
                    UpdateSendButtonState();
                    RenderProjects();
                    RefreshPromptQueue();
                    if (pending.Message.Status == "completed" && !pending.StopRequested && !pending.PauseConfirmed)
                        _ = DrainQueuedPromptsAsync(pending.ConversationId);
                }
            }
            // Formal terminal state releases execution first; display-cache maintenance cannot hold the queue.
            // 正式终态先释放执行状态；展示缓存维护不能拖住发送和后续队列。
            if (cachePresentation) await CacheReplyPresentationAsync(pending);
        }
    }

    private async Task CacheReplyPresentationAsync(PendingChatReply pending)
    {
        try
        {
            await SaveReplyTimeAsync(pending.ConversationId, pending.Message);
            await MaintainHistoryAfterReplyAsync(pending.ConversationId);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or InvalidOperationException
            or System.Text.Json.JsonException or System.Runtime.InteropServices.COMException)
        {
            // Display-cache failures must not alter committed replies or stop the queue.
            // 展示缓存失败不改已提交回复，也不阻断后续队列。
            Debug.WriteLine($"Reply presentation cache deferred: {error.GetType().Name}");
        }
    }

    private async Task SavePendingUserMessageAsync(PendingChatReply pending)
    {
        pending.Cancellation.Token.ThrowIfCancellationRequested();
        var (chat, project) = FindChat(pending.ConversationId);
        if (_chatClosing || chat is null || !_pendingReplies.TryGetValue(pending.ConversationId, out var current) || current != pending ||
            !chat.Messages.Any(message => message.Id == pending.UserMessageId && message.Role == "user" && message.Content == pending.Question))
        {
            pending.Cancellation.Cancel();
            throw new OperationCanceledException(pending.Cancellation.Token);
        }

        // The gateway commits existing text-only chats by stable ID; new scopes and attachment owners are saved first.
        // 已有聊天纯文字由流式入口按稳定 ID 正式提交；新聊天归属与附件所属消息仍先保存。
        if (!chat.IsPersisted || _messageAttachmentDrafts.ContainsKey(pending.UserMessageId))
            await SaveChatChangesAsync(project);
        pending.Cancellation.Token.ThrowIfCancellationRequested();
        var user = chat.Messages.First(message => message.Id == pending.UserMessageId);
        await PrepareMessageAttachmentsAsync(pending.ConversationId, user, pending.Cancellation.Token);
        pending.Cancellation.Token.ThrowIfCancellationRequested();
        var owner = FindChat(pending.ConversationId);
        if (_chatClosing || owner.Chat != chat || owner.Project?.Id != project?.Id ||
            !_pendingReplies.TryGetValue(pending.ConversationId, out current) || current != pending ||
            !chat.Messages.Any(message => message.Id == pending.UserMessageId && message.Role == "user" && message.Content == pending.Question))
        {
            pending.Cancellation.Cancel();
            throw new OperationCanceledException(pending.Cancellation.Token);
        }
    }

    private static void StartReplyTimer(PendingChatReply pending)
    {
        // Start timing at the first model segment, not transport acceptance or later rounds.
        // 首个模型段开始计时；接受传输不启表，后续轮次不重置。
        pending.Message.StartGenerationTiming();
    }

    private static void AcceptReplyTextSnapshot(PendingChatReply pending, string? content, string? reasoning)
    {
        pending.Message.Content = content ?? string.Empty;
        pending.Message.Reasoning = reasoning ?? string.Empty;
        pending.Content.Clear(); pending.Content.Append(pending.Message.Content);
        pending.Reasoning.Clear(); pending.Reasoning.Append(pending.Message.Reasoning);
    }

    private static void SettleReplySegments(PendingChatReply pending)
    {
        if (pending.Message.Status == "streaming") return;
        var segments = pending.Message.AssistantSegments;
        for (int index = 0; index < segments.Count; index++)
            if (segments[index].Status == "streaming")
                segments[index] = segments[index] with { Status = pending.Message.Status == "completed" ? "completed" : "interrupted" };
    }

    private static void AcceptAssistantSegment(PendingChatReply pending, AssistantSegment segment)
    {
        var segments = pending.Message.AssistantSegments;
        int index = segments.FindIndex(value => value.Id == segment.Id);
        if (index < 0) segments.Add(segment);
        else segments[index] = segment;
        pending.SegmentText[segment.Id] = (new StringBuilder(segment.Content), new StringBuilder(segment.Reasoning));
        pending.DirtySegments.Remove(segment.Id);
        if (segment.Status != "streaming") pending.ThinkingTime.Stop();
        pending.Dirty = true;
    }

    private static void AppendSegmentDelta(PendingChatReply pending, ChatStreamEvent update, bool reasoning)
    {
        if (update.SegmentId is null) return;
        if (!pending.SegmentText.TryGetValue(update.SegmentId, out var text)) return;
        (reasoning ? text.Reasoning : text.Content).Append(update.Delta);
        pending.DirtySegments.Add(update.SegmentId);
    }

    private async void Transcript_RetryRequested(object? sender, Guid messageId)
    {
        var failed = ActiveMessages.FirstOrDefault(row => row.Message.Id == messageId);
        if (_chatClosing || _savingOnClose || StoragePaths.IsMigrating || failed is null || IsReplyInProgress(failed.ConversationId)) return;
        var (chat, project) = FindChat(failed.ConversationId);
        if (chat is null || chat.Messages.LastOrDefault() != failed.Message || !_retryingReplies.Add(chat.Id)) return;
        try
        {
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(60));
            var detail = await FindReplyTaskAsync(chat, project?.Id, failed.Message, deadline.Token);
            if (_chatClosing || IsReplyInProgress(chat.Id) || FindChat(chat.Id).Chat != chat) return;
            if (detail is not null)
            {
                // Retry continues the original task and receipts; its old reply remains visible.
                // 重试续接原任务和回执，已有回复保留，不重发原问题或删除正文。
                var receipt = await _tasksApiClient.ControlAsync(chat.Id, detail.Task.TaskId, detail.Task.Version,
                    "resume", project?.Id, deadline.Token);
                await ObserveResumedTaskAsync(receipt.Task, detail.Checkpoint, expectedReplyId: failed.Message.Id);
                return;
            }
            if (failed.ToolActivities.Count > 0) return;
            var user = chat.Messages.Take(chat.Messages.IndexOf(failed.Message)).LastOrDefault(message => message.Role == "user");
            if (user is null) return;
            var pending = new PendingChatReply(chat.Id, user.Id, user.Content,
                failed.Message.Provider, failed.Message.Model, _layout.PermissionMode, failed.Message)
                { StreamRequestId = failed.Message.Id, Presentation = failed };
            pending.Content.Append(failed.Message.Content); pending.Reasoning.Append(failed.Message.Reasoning);
            failed.Message.Status = "streaming"; failed.Message.Error = string.Empty;
            _pendingReplies.Add(chat.Id, pending); StartReplyRefreshTimer(); UpdateSendButtonState(); failed.Refresh();
            await ReceiveReplyAsync(pending);
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException or InvalidOperationException
            or InvalidDataException or System.Text.Json.JsonException)
        { if (!_chatClosing) ShowActionFeedback(UiText.Get("消息已保留，请重试。")); }
        finally { _retryingReplies.Remove(chat.Id); }
    }

    private void CancelPendingReply(Guid chatId)
    {
        if (!_pendingReplies.Remove(chatId, out var pending)) return;
        pending.ThinkingTime.Stop();
        pending.Message.Status = "interrupted";
        MessageTimePresentation.RecordEnd(pending.ConversationId, pending.Message, DateTimeOffset.UtcNow);
        FlushReply(pending, final: true);
        pending.Cancellation.Cancel();
        RenderProjects();
    }

    private void PauseRepliesForClose()
    {
        _chatClosing = true;
        CancelModelPickerRefresh();
        _replyRefreshTimer?.Stop();
        foreach (var pending in _pendingReplies.Values.ToArray())
        {
            pending.Cancellation.Cancel();
            pending.ThinkingTime.Stop();
            if (pending.Message.Status == "streaming") pending.Message.Status = "interrupted";
            FlushReply(pending, final: true);
        }
        _toolResultCancellation?.Cancel();
        TaskPanel.SetPreviewEnabled(false);
    }

    private void StopReplies()
    {
        PauseRepliesForClose();
        _modelApiClient.Dispose();
        ScreenshotPanel.Dispose();
        DocumentPanel.Dispose();
        WorkspaceTerminal.Dispose();
        WorkTabs.Dispose();
        TaskPanel.Dispose();
        _tasksApiClient.Dispose();
        _attachmentsApiClient.Dispose();
        MountedWorkspace.Dispose();
        _agentApiClient.Dispose();
        ConversationMessages.Dispose();
    }
}
