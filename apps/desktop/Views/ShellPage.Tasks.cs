using KYNXA_Desktop.Services;
using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private readonly TasksApiClient _tasksApiClient = new();
    private readonly Dictionary<Guid, Guid> _pausedTaskIds = [];

    private void InitializeTaskPanel()
    {
        TaskPanel.ConfigureApi(_tasksApiClient);
        TaskPanel.SetPreviewEnabled(false);
        Unloaded += (_, _) => TaskPanel.SetPreviewEnabled(false);
    }

    private void SynchronizeTaskPanel(bool sidebarVisible)
    {
        NotifyApprovalConversationChanged();
        Guid? projectId = ActiveChatId is Guid chat ? FindChat(chat).Project?.Id : null;
        TaskPanel.ShowConversation(ActiveChatId, projectId);
        // The collapsed header does not poll; expanding this view never changes the task itself.
        // 折叠标题不发起轮询；展开查看不会改变正式任务及暂停恢复状态。
        TaskPanel.SetPreviewEnabled(sidebarVisible && !_chatClosing);
    }

    private async Task StopPendingReplyAsync(PendingChatReply pending)
    {
        pending.StopRequested = true;
        if (pending.StopControlPending || _chatClosing) return;
        pending.StopControlPending = true;
        bool waitingRegistration = false;
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(15));
        try
        {
            if (pending.TaskId is not Guid taskId)
            {
                waitingRegistration = true;
                return;
            }
            var (_, project) = FindChat(pending.ConversationId);
            var detail = await _tasksApiClient.GetAsync(pending.ConversationId, taskId, project?.Id, deadline.Token);
            if (TaskStates.IsTerminal(detail.Task.State)) return;
            var receipt = await _tasksApiClient.ControlAsync(pending.ConversationId, taskId, detail.Task.Version,
                "pause", project?.Id, deadline.Token);
            pending.PauseConfirmed = receipt.Task.State == "paused";
            if (!pending.PauseConfirmed) throw new InvalidDataException(UiText.Get("任务暂停结果尚未确认，请查看任务状态。"));
            _pausedTaskIds[pending.ConversationId] = taskId;
            if (pending.Message.Status != "streaming")
            {
                pending.Message.Error = string.Empty;
                FlushReply(pending, final: true);
            }
            if (!_chatClosing && _pendingReplies.TryGetValue(pending.ConversationId, out var current) && current == pending)
                pending.Cancellation.Cancel();
            TaskPanel.NotifyChanged(pending.ConversationId);
        }
        catch (GatewayApiException error) when (error.StatusCode == System.Net.HttpStatusCode.NotFound)
        {
            // Registration and stream acceptance can race; pause once the durable task identity arrives.
            // 正式任务登记与流接受可能并发；收到持久任务身份后再暂停，不能替换为取消。
            waitingRegistration = true;
        }
        catch (Exception error) when (error is System.Net.Http.HttpRequestException or OperationCanceledException
            or InvalidOperationException or InvalidDataException or System.Text.Json.JsonException)
        {
            TaskPanel.NotifyChanged(pending.ConversationId);
        }
        finally
        {
            pending.StopControlPending = false;
            if (waitingRegistration && pending.Started && pending.StopRegistrationRetries++ < 1 && !_chatClosing)
                _ = StopPendingReplyAsync(pending);
        }
    }

    private async Task<bool> TrySteerPausedTaskAsync()
    {
        if (ActiveChatId is not Guid conversation || (string.IsNullOrWhiteSpace(PromptTextBox.Text) && !HasAttachmentDrafts)) return false;
        if (_pausedTaskIds.ContainsKey(conversation)) return TryQueuePrompt();
        var (chat, owner) = FindChat(conversation);
        // An empty new draft has no formal chat or task yet; persisted chats remain eligible even before history loads.
        // 空的新聊天草稿尚无正式聊天及任务；已持久化的旧聊天即使历史未加载也必须查询。
        if (chat is null || !chat.CanPersist) return false;
        string draft = PromptTextBox.Text;
        Guid? project = owner?.Id;
        Guid? recoveredTaskId = null;
        // Own the send gate during recovery; a double click or a changed chat must not start another request.
        // 恢复查询独占发送入口；双击或切换聊天不能创建另一请求，也不扫描消息日志。
        _sendingPrompt = true;
        UpdateSendButtonState();
        try
        {
            var response = await _tasksApiClient.PausedAsync(conversation, project);
            if (_chatClosing || _savingOnClose || ActiveChatId != conversation || PromptTextBox.Text != draft
                || FindChat(conversation).Project?.Id != project) return true;
            var selected = PausedTaskSelection.From(response);
            if (selected.IsAmbiguous)
            {
                ShowActionFeedback(UiText.Get("此聊天有多个暂停任务，请在任务侧栏选择要继续的任务；输入内容已保留。"));
                return true;
            }
            recoveredTaskId = selected.TaskId;
            if (recoveredTaskId is Guid paused) _pausedTaskIds[conversation] = paused;
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException
            or InvalidOperationException or InvalidDataException or System.Text.Json.JsonException or ArgumentException)
        {
            if (!_chatClosing && ActiveChatId == conversation)
                ShowActionFeedback(UiText.Get("暂停任务读取失败，输入内容已保留；请重试或在任务侧栏查看。"));
            return true;
        }
        finally { _sendingPrompt = false; UpdateSendButtonState(); }
        return recoveredTaskId is not null && TryQueuePrompt();
    }

    private async Task<bool> SendTaskSteeringAsync(Guid conversation, QueuedPrompt prompt)
    {
        if (_savingOnClose || _chatClosing) return false;
        _pendingReplies.TryGetValue(conversation, out var pending);
        Guid? taskId = pending?.TaskId;
        if (taskId is null && _pausedTaskIds.TryGetValue(conversation, out var paused)) taskId = paused;
        var (chat, project) = FindChat(conversation);
        if (chat is null) return false;
        var previous = pending?.Message ?? chat.Messages.LastOrDefault(message => message.Role == "assistant"
            && message.SupersededByUserMessageId is null && message.Status != "completed");
        string? previousStatus = previous?.Status;
        var submitted = PrepareSteeringReply(conversation, prompt, pending);
        bool steeringAccepted = false;
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(60));
        try
        {
            await PrepareMessageAttachmentsAsync(conversation, submitted, deadline.Token);
            prompt.Attachments = submitted.Attachments.ToList();
            if (taskId is null && pending is not null)
                taskId = await pending.TaskRegistered.Task.WaitAsync(deadline.Token);
            if (taskId is null || _chatClosing) throw new InvalidOperationException("The task has no active continuation.");
            var current = await _tasksApiClient.GetAsync(conversation, taskId.Value, project?.Id, deadline.Token);
            if (TaskStates.IsTerminal(current.Task.State))
            {
                _pausedTaskIds.Remove(conversation);
                throw new InvalidOperationException("The task has already ended.");
            }
            TaskControlResponse receipt;
            try
            {
                receipt = await _tasksApiClient.SteerAsync(conversation, taskId.Value, current.Task.Version,
                    submitted.Id, prompt.Text, "continue", project?.Id, deadline.Token, replyMessageId: prompt.ReplyMessageId,
                    attachments: submitted.Attachments.ToArray());
            }
            catch (GatewayApiException error) when (error.StatusCode == System.Net.HttpStatusCode.Conflict
                && error.ErrorCode == "TASK_VERSION_CONFLICT")
            {
                // Refresh only a rejected version; keep the same task and message identities.
                // 仅对已明确拒绝的旧版本刷新一次，保持任务及引导消息身份，不重放未知操作。
                current = await _tasksApiClient.GetAsync(conversation, taskId.Value, project?.Id, deadline.Token);
                if (TaskStates.IsTerminal(current.Task.State)) throw;
                receipt = await _tasksApiClient.SteerAsync(conversation, taskId.Value, current.Task.Version,
                    submitted.Id, prompt.Text, "continue", project?.Id, deadline.Token, replyMessageId: prompt.ReplyMessageId,
                    attachments: submitted.Attachments.ToArray());
            }
            steeringAccepted = true;
            TaskPanel.NotifyChanged(conversation);
            if (receipt.Task.State == "paused")
            {
                _pausedTaskIds[conversation] = taskId.Value;
                return true;
            }
            _pausedTaskIds.Remove(conversation);
            if (receipt.ReconnectRequired && pending is not null)
            {
                pending.PauseConfirmed = true;
                pending.Cancellation.Cancel();
            }
            if (pending is null || receipt.ReconnectRequired) await ObserveResumedTaskAsync(receipt.Task);
            return true;
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException
            or InvalidOperationException or InvalidDataException or System.Text.Json.JsonException or ArgumentException)
        {
            // A failed observation cannot put an already accepted guidance back in the queue.
            // 接续观察失败不能把已接受的引导退回队列，正式补充与当前回复位置仍保留。
            if (steeringAccepted)
            {
                TaskPanel.NotifyChanged(conversation);
                return true;
            }
            chat.Messages.RemoveAll(message => message.Id == submitted.Id || message.Id == prompt.ReplyMessageId);
            if (previous is not null)
            {
                previous.SupersededByUserMessageId = null;
                previous.Status = previousStatus ?? "interrupted";
                if (pending is not null) { SetPendingReplyMessage(pending, previous); pending.WaitingReplyAnchorId = null; }
            }
            if (ActiveChatId == conversation) UpdateConversationPresentation(openAtBottom: false);
            if (!_chatClosing) ShowActionFeedback(UiText.Get("任务补充未确认，原任务进度已保留，请查看状态。"));
            return false;
        }
    }
    private async Task ObserveResumedTaskAsync(TaskRecord task)
    {
        if (_chatClosing || task.ParentTaskId is not null || task.State is not ("queued" or "running")
            || !Guid.TryParse(task.RequestId, out var requestId)) return;
        if (_pendingReplies.TryGetValue(task.Scope.ConversationId, out var prior))
        {
            if (prior.TaskId != task.TaskId || !prior.PauseConfirmed) return;
            try { await prior.Finished.Task.WaitAsync(TimeSpan.FromSeconds(15)); }
            catch (TimeoutException)
            {
                if (!_chatClosing) ShowActionFeedback(UiText.Get("任务已继续，打开原聊天可查看回复。"));
                return;
            }
        }
        var (chat, _) = FindChat(task.Scope.ConversationId);
        var rootMessage = chat?.Messages.FirstOrDefault(item => item.Id == requestId && item.Role == "assistant");
        if (chat is null || rootMessage is null)
        {
            if (!_chatClosing) ShowActionFeedback(UiText.Get("任务已继续，打开原聊天可查看回复。"));
            return;
        }
        int position = chat.Messages.IndexOf(rootMessage);
        var original = chat.Messages.Take(position).LastOrDefault(item => item.Role == "user");
        if (original is null) return;
        var message = chat.Messages.LastOrDefault(item => item.Role == "assistant"
            && item.SupersededByUserMessageId is null && (item.Id == requestId || item.OriginUserMessageId == original.Id))
            ?? rootMessage;
        try
        {
            await MessageTimePresentation.ForgetAsync(chat.Id, message);
            if (_chatClosing || _pendingReplies.ContainsKey(chat.Id)) return;
            var pending = new PendingChatReply(chat.Id, original.Id, original.Content, message.Provider, message.Model,
                _layout.PermissionMode, message) { TaskId = task.TaskId, Started = true, StreamRequestId = requestId };
            pending.TaskRegistered.TrySetResult(task.TaskId);
            message.Status = "streaming"; message.Error = string.Empty;
            pending.Content.Append(message.Content); pending.Reasoning.Append(message.Reasoning);
            foreach (var segment in message.AssistantSegments)
                pending.SegmentText[segment.Id] = (new System.Text.StringBuilder(segment.Content), new System.Text.StringBuilder(segment.Reasoning));
            pending.Presentation = new ConversationMessageViewModel(chat.Id, message);
            _pendingReplies.Add(chat.Id, pending);
            _pausedTaskIds.Remove(chat.Id);
            StartReplyRefreshTimer();
            if (ActiveChatId == chat.Id) UpdateConversationPresentation(openAtBottom: false);
            UpdateSendButtonState();
            _ = ReceiveReplyAsync(pending, task.TaskId);
        }
        catch (Exception error) when (error is InvalidOperationException or IOException or UnauthorizedAccessException)
        { if (!_chatClosing) ShowActionFeedback(UiText.Get("任务已继续，打开原聊天可查看回复。")); }
    }
}
