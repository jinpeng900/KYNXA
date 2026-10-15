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
        public bool Dirty { get; set; }
        public bool Started { get; set; }
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
        bool active = ActiveChatId is Guid id && _pendingReplies.ContainsKey(id);
        bool hasInput = !string.IsNullOrWhiteSpace(PromptTextBox.Text) || HasAttachmentDrafts;
        SendButton.IsEnabled = active || (!_sendingPrompt && _projectsReady && !_projectActionPending
            && hasInput);
        // Retain the shared theme brush while the durable-task action determines the visible icon.
        // 保留共享主题画刷；可见图标由持久任务的当前动作决定。
        SendArrow.Stroke = AppearanceService.GetBrush(SendButton.IsEnabled ? "KynxaOnAccentBrush" : "KynxaDisabledTextBrush");
        bool pause = active && !hasInput;
        SendArrow.Visibility = pause ? Visibility.Collapsed : Visibility.Visible;
        StopReplyIcon.Visibility = pause ? Visibility.Visible : Visibility.Collapsed;
        string caption = pause ? "暂停并保存进度" : active ? "加入等待队列" : "发送";
        AutomationProperties.SetName(SendButton, UiText.Get(caption));
        ToolTipService.SetToolTip(SendButton, UiText.Get(caption));
    }

    private PendingChatReply BeginPendingReply(Guid chatId, string question, string? provider, string? model, string? permissionMode = null, Guid? requestId = null)
    {
        var (chat, _) = FindChat(chatId);
        if (chat is null || _pendingReplies.ContainsKey(chatId)) throw new InvalidOperationException(UiText.Get("当前聊天正在生成回复。"));
        var user = chat.Messages.LastOrDefault(message => message.Role == "user")
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
        foreach (string id in pending.DirtySegments)
        {
            int index = pending.Message.AssistantSegments.FindIndex(segment => segment.Id == id);
            if (index >= 0 && pending.SegmentText.TryGetValue(id, out var text))
                pending.Message.AssistantSegments[index] = pending.Message.AssistantSegments[index] with
                { Content = text.Content.ToString(), Reasoning = text.Reasoning.ToString() };
        }
        pending.DirtySegments.Clear();
        pending.Message.Content = pending.Content.ToString();
        pending.Message.Reasoning = pending.Reasoning.ToString();
        pending.Message.ReasoningDurationMs = pending.ThinkingTime.ElapsedMilliseconds;
        pending.Presentation.Refresh(pending.ThinkingTime.IsRunning);
        pending.Dirty = false;
    }

    private async Task ReceiveReplyAsync(PendingChatReply pending, Guid? observedTaskId = null)
    {
        try
        {
            // Storage and connection preparation follow the already visible user and waiting rows.
            // 用户消息与等待占位已显示后再保存与连接；失败仍走本轮错误收尾，不派发未保存的请求。
            if (observedTaskId is null) await SavePendingUserMessageAsync(pending);
            if (pending.StopRequested) pending.Cancellation.Cancel();
            pending.Cancellation.Token.ThrowIfCancellationRequested();
            var updates = observedTaskId is Guid taskId
                ? _modelApiClient.StreamTaskReplyAsync(pending.ConversationId, taskId, pending.StreamRequestId, pending.Cancellation.Token)
                : _modelApiClient.StreamReplyAsync(new ChatRequest(pending.ConversationId, pending.Question, pending.Model,
                    pending.PermissionMode, pending.Provider, pending.StreamRequestId, pending.UserMessageId,
                    FindChat(pending.ConversationId).Chat?.Messages.FirstOrDefault(message => message.Id == pending.UserMessageId)
                        ?.Attachments.Select(file => file.Id).ToArray()), pending.Cancellation.Token);
            await foreach (var update in updates)
            {
                if (_chatClosing || !_pendingReplies.TryGetValue(pending.ConversationId, out var current) || current != pending) return;
                if (pending.WaitingReplyAnchorId is not null && update.Type is "reasoning_delta" or "text_delta"
                    or "assistant_segment" or "content_snapshot") continue;
                switch (update.Type)
                {
                    case "reply_anchor":
                        AcceptReplyAnchor(pending, update);
                        break;
                    case "started":
                        pending.Started = true;
                        pending.TaskId = update.RootRequestTaskId ?? update.TaskId ?? pending.TaskId;
                        pending.TaskRegistered.TrySetResult(pending.TaskId);
                        if (update.TaskId is not null) TaskPanel.NotifyChanged(pending.ConversationId);
                        if (pending.StopRequested) await StopPendingReplyAsync(pending);
                        break;
                    case "task_progress":
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
                    case "tool_call":
                    case "tool_result":
                        if (update.Tool is { } activity)
                        {
                            if (pending.WaitingReplyAnchorId is null) AcceptToolActivity(pending, activity);
                        }
                        break;
                    case "terminal_output":
                        continue; // Command output stays in the formal tool receipt; it has no sidebar projection.
                        // 命令输出保留在正式工具回执中，不生成侧栏展示投影。
                    case "approval_required":
                        if (update.Tool is { } approval)
                        {
                            AcceptToolActivity(pending, approval);
                            FlushReply(pending);
                            QueueToolApproval(pending, approval);
                        }
                        break;
                    case "reasoning_delta":
                        StartReplyTimer(pending);
                        pending.ThinkingTime.Start();
                        pending.Reasoning.Append(update.Delta);
                        AppendSegmentDelta(pending, update, reasoning: true);
                        break;
                    case "text_delta":
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
                        if (!string.IsNullOrEmpty(update.Content) || pending.Content.Length == 0)
                        { pending.Content.Clear(); pending.Content.Append(update.Content); }
                        if (!string.IsNullOrEmpty(update.Reasoning) || pending.Reasoning.Length == 0)
                        { pending.Reasoning.Clear(); pending.Reasoning.Append(update.Reasoning); }
                        break;
                    case "completed":
                    case "interrupted":
                    case "error":
                        pending.ThinkingTime.Stop();
                        if (!string.IsNullOrEmpty(update.Content) || pending.Content.Length == 0)
                        { pending.Content.Clear(); pending.Content.Append(update.Content); }
                        if (update.Reasoning is not null) { pending.Reasoning.Clear(); pending.Reasoning.Append(update.Reasoning); }
                        if (update.AssistantSegments is { } segments && (segments.Any() || pending.Message.AssistantSegments.Count == 0))
                        {
                            pending.Message.AssistantSegments.Clear();
                            pending.Message.AssistantSegments.AddRange(segments);
                            pending.SegmentText.Clear();
                            pending.DirtySegments.Clear();
                        }
                        pending.Message.Status = update.Type == "completed" ? "completed" : update.Type;
                        pending.Message.Error = update.Error ?? string.Empty;
                        pending.Message.DurationMs = update.DurationMs;
                        pending.Message.TaskCompletionState = ChatMessageState.ReadTaskCompletion(update.TaskCompletion);
                        if (update.Code == "TASK_PAUSED" && pending.TaskId is Guid pausedTaskId)
                        {
                            pending.PauseConfirmed = true;
                            _pausedTaskIds[pending.ConversationId] = pausedTaskId;
                            pending.Message.Error = string.Empty;
                        }
                        MessageTimePresentation.RecordEnd(pending.ConversationId, pending.Message, DateTimeOffset.UtcNow);
                        FlushReply(pending, final: true);
                        await SaveReplyTimeAsync(pending.ConversationId, pending.Message);
                        break;
                }
                pending.Dirty = true;
            }
            if (pending.Message.Status == "streaming")
            {
                pending.Message.Status = "interrupted";
                pending.Message.Error = UiText.Get("连接已结束，回复尚未完成。");
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
                if (pending.Message.Status != "completed")
                {
                    var segments = pending.Message.AssistantSegments;
                    for (int index = 0; index < segments.Count; index++)
                        if (segments[index].Status == "streaming") segments[index] = segments[index] with { Status = "interrupted" };
                }
                if (_pendingReplies.TryGetValue(pending.ConversationId, out var current) && current == pending)
                {
                    MessageTimePresentation.RecordEnd(pending.ConversationId, pending.Message, DateTimeOffset.UtcNow);
                    FlushReply(pending, final: true);
                    _pendingReplies.Remove(pending.ConversationId);
                    await SaveReplyTimeAsync(pending.ConversationId, pending.Message);
                    await MaintainHistoryAfterReplyAsync(pending.ConversationId);
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
                    UpdateSendButtonState();
                    RenderProjects();
                    RefreshPromptQueue();
                    if (pending.Message.Status == "completed" && !pending.StopRequested && !pending.PauseConfirmed)
                        _ = DrainQueuedPromptsAsync(pending.ConversationId);
                }
            }
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

        // The gateway owns the commit; retries keep the same user/request IDs and original work scope.
        // 正式提交仍由网关负责；重试沿用用户与请求 ID，并先保存原工作归属，避免未保存的新工作聊天变成普通聊天。
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
        // The first model segment follows preparation; transport acceptance and later rounds do not restart the clock.
        // 首个模型消息段表示准备已完成；传输接受事件不开始计时，后续轮次也不重置计时。
        pending.Message.GenerationStartedTimestamp ??= Stopwatch.GetTimestamp();
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
        if (_chatClosing || _savingOnClose || StoragePaths.IsMigrating || failed is null ||
            failed.ToolActivities.Count > 0 || IsReplyInProgress(failed.ConversationId)) return;
        var (chat, _) = FindChat(failed.ConversationId);
        if (chat is null || chat.Messages.LastOrDefault() != failed.Message) return;
        string? question = chat.Messages.Take(chat.Messages.Count - 1).LastOrDefault(message => message.Role == "user")?.Content;
        if (question is null || !_retryingReplies.Add(chat.Id)) return;
        PendingChatReply pending;
        try
        {
            // Reserve this retry across cache I/O, then recheck ownership before replacing its source row.
            // 缓存读写期间保留重试占位，替换原消息前再次核对会话和请求归属。
            if (!await MessageTimePresentation.ForgetAsync(chat.Id, failed.Message))
                ShowActionFeedback(UiText.Get("回复时间未能保存到本机缓存，聊天内容不受影响。"));
            if (_chatClosing || _savingOnClose || StoragePaths.IsMigrating || IsReplyInProgress(chat.Id) ||
                FindChat(chat.Id).Chat != chat || chat.Messages.LastOrDefault() != failed.Message) return;
            chat.Messages.Remove(failed.Message);
            pending = BeginPendingReply(chat.Id, question,
                string.IsNullOrEmpty(failed.Message.Provider) ? _selectedModel?.ProviderId : failed.Message.Provider,
                string.IsNullOrEmpty(failed.Message.Model) ? _selectedModel?.ModelId : failed.Message.Model, requestId: failed.Message.Id);
            if (ActiveChatId == chat.Id)
            {
                int index = ActiveMessages.IndexOf(failed);
                if (index >= 0)
                {
                    ActiveMessages[index] = pending.Presentation;
                    ConversationMessages.ShowConversation(chat.Id, ActiveMessages.ToArray(), openAtBottom: false);
                    ScreenshotPanel.ShowConversation(chat.Id, ActiveMessages.ToArray());
                }
                else UpdateConversationPresentation();
            }
            UpdateSendButtonState();
        }
        finally { _retryingReplies.Remove(chat.Id); }
        await ReceiveReplyAsync(pending);
    }

    private void CancelPendingReply(Guid chatId)
    {
        if (!_pendingReplies.Remove(chatId, out var pending)) return;
        pending.ThinkingTime.Stop();
        pending.Message.Content = pending.Content.ToString();
        pending.Message.Reasoning = pending.Reasoning.ToString();
        pending.Message.ReasoningDurationMs = pending.ThinkingTime.ElapsedMilliseconds;
        pending.Message.Status = "interrupted";
        MessageTimePresentation.RecordEnd(pending.ConversationId, pending.Message, DateTimeOffset.UtcNow);
        pending.Presentation.Refresh();
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
