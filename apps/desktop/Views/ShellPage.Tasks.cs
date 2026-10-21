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
    private readonly HashSet<Guid> _resumingTaskConversations = [];
    private sealed record PausedTaskRecovery(Guid ConversationId, Guid? ProjectId, Task<PausedTaskSelection?> Selection);
    private PausedTaskRecovery? _pausedTaskRecovery;

    private void InitializeTaskPanel()
    {
        TaskPanel.ConfigureApi(_tasksApiClient);
        TaskPanel.ResumeRequested += (_, task) => _ = ResumeSelectedTaskAsync(task);
        TaskPanel.SetPreviewEnabled(false);
        Unloaded += (_, _) => TaskPanel.SetPreviewEnabled(false);
    }

    private async Task ResumeSelectedTaskAsync(TaskRecord selected, string[]? unknownOperationIds = null,
        TaskModelReservationIdentity[]? unknownReservationIds = null, TaskOperationIdentity[]? unknownOperations = null)
    {
        Guid conversation = selected.Scope.ConversationId;
        if (_chatClosing || _savingOnClose || ActiveChatId != conversation || selected.ParentTaskId is not null ||
            !_resumingTaskConversations.Add(conversation)) return;
        TaskRecord current = selected;
        bool CanControlSelected() => CanResumeSelectedTask(selected) ||
            ((unknownOperations is not null || unknownReservationIds is not null) && selected.State == "waiting-verification" &&
            ActiveChatId == conversation && _pendingReplies.TryGetValue(conversation, out var live) && live.TaskId == selected.TaskId);
        string? feedback = null;
        TaskPanel.SetResumePending(selected, true);
        try
        {
            if (!CanControlSelected())
            {
                feedback = UiText.Get("当前回复仍在处理，请稍后继续所选任务。");
                return;
            }
            // Selection is explicit and remains the same root identity; it does not send the input draft as a new request.
            // 用户明确选择原根任务；选择不提交输入草稿，也不建立新的用户消息或助手占位。
            _pausedTaskIds[conversation] = selected.TaskId;
            _pausedTaskRecovery = new(conversation, selected.Scope.ProjectId,
                Task.FromResult<PausedTaskSelection?>(new(selected.TaskId, false)));
            if (!await EnsureSelectedTaskHistoryAsync(selected))
            {
                feedback = UiText.Get("原任务回复尚未加载，进度已保留，请刷新后继续。");
                return;
            }
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(60));
            var detail = await _tasksApiClient.GetAsync(conversation, selected.TaskId, selected.Scope.ProjectId, deadline.Token);
            current = detail.Task;
            // History and version reads yield to sends and navigation; recheck immediately before controlling the old root.
            // 历史及版本读取期间允许发送和导航；控制旧根前重新核对当前聊天和活动回复，不能使用过期选择。
            if (!CanControlSelected())
            {
                feedback = UiText.Get("当前回复仍在处理，请稍后继续所选任务。");
                return;
            }
            // Confirm only operations the fresh detail still lists as unknown; otherwise use the ordinary resume.
            // 只确认最新详情中仍未核验的操作；已无此类操作时按普通继续处理，不扩大用户确认范围。
            string[] acknowledged = unknownOperationIds is null || current.State != "waiting-verification" ? [] :
                (detail.RecoveryOperations ?? detail.Operations).Where(TaskOperationStates.NeedsUnknownAcknowledgement)
                    .Select(operation => operation.Intent.OperationId).Intersect(unknownOperationIds).ToArray();
            var freshReservations = (detail.RecoveryModelReservations ?? [])
                .Where(item => item.State == "unknown" && (unknownReservationIds ?? [])
                    .Contains(new TaskModelReservationIdentity(item.TaskId, item.ReservationId)))
                .Select(item => new TaskModelReservationIdentity(item.TaskId, item.ReservationId)).ToArray();
            var scopedOperations = (detail.RecoveryOperations ?? detail.Operations)
                .Where(TaskOperationStates.NeedsUnknownAcknowledgement)
                .Select(item => new TaskOperationIdentity(item.TaskId, item.Intent.OperationId))
                .Intersect(unknownOperations ?? []).ToArray();
            if (TaskStates.CanResume(current.State))
            {
                // Cost and operation acknowledgements remain independently auditable; re-read the root version between them.
                // 成本与操作确认独立留痕；两者并存时使用前一步回执的新版本，避免覆盖并发控制。
                TaskControlResponse? costReceipt = null;
                if (freshReservations.Length > 0)
                {
                    costReceipt = await _tasksApiClient.AcknowledgeUnknownCostsAsync(conversation, current.TaskId,
                        current.Version, freshReservations, current.Scope.ProjectId, deadline.Token);
                    current = costReceipt.Task;
                }
                var receipt = scopedOperations.Length > 0
                    ? await _tasksApiClient.AcknowledgeUnknownScopedAsync(conversation, current.TaskId, current.Version,
                        scopedOperations, current.Scope.ProjectId, deadline.Token)
                    : acknowledged.Length > 0 && unknownOperations is null
                    ? await _tasksApiClient.AcknowledgeUnknownAsync(conversation, current.TaskId, current.Version,
                        acknowledged, current.Scope.ProjectId, deadline.Token)
                    : costReceipt ?? await _tasksApiClient.ControlAsync(conversation, current.TaskId, current.Version,
                        "resume", current.Scope.ProjectId, deadline.Token);
                current = receipt.Task;
                OnUnknownOutcomeControlled(conversation, current);
                if (receipt.Recovery is { ValueKind: System.Text.Json.JsonValueKind.Object } recovery &&
                    recovery.TryGetProperty("pending", out var pending) && pending.ValueKind == System.Text.Json.JsonValueKind.Array)
                {
                    feedback = string.Join("\n", pending.EnumerateArray().Where(item => item.ValueKind == System.Text.Json.JsonValueKind.Object &&
                        item.TryGetProperty("message", out var text) && text.ValueKind == System.Text.Json.JsonValueKind.String)
                        .Select(item => item.GetProperty("message").GetString()));
                }
            }
            if (current.State is "queued" or "running")
            {
                feedback = null;
                await ObserveResumedTaskAsync(current);
            }
            else if (string.IsNullOrWhiteSpace(feedback)) feedback = current.StopReason;
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException or InvalidOperationException or
            InvalidDataException or IOException or UnauthorizedAccessException or System.Text.Json.JsonException or ArgumentException)
        {
            feedback = error is GatewayApiException ? error.Message : UiText.Get("任务控制未确认，请刷新状态后再操作。");
        }
        finally
        {
            _resumingTaskConversations.Remove(conversation);
            if (!_chatClosing)
            {
                TaskPanel.SetResumePending(current, false, feedback);
                TaskPanel.NotifyChanged(conversation);
            }
        }
    }

    private bool CanResumeSelectedTask(TaskRecord task)
    {
        var owner = FindChat(task.Scope.ConversationId);
        bool hasActiveReply = _pendingReplies.TryGetValue(task.Scope.ConversationId, out var pending);
        return !_chatClosing && !_savingOnClose && owner.Chat is not null &&
            PausedTaskSelection.CanResumeInContext(task, ActiveChatId, owner.Project?.Id,
                hasActiveReply, pending?.TaskId, pending?.PauseConfirmed == true);
    }

    private async Task<bool> EnsureSelectedTaskHistoryAsync(TaskRecord task)
    {
        var (chat, _) = FindChat(task.Scope.ConversationId);
        if (chat is null || !Guid.TryParse(task.RequestId, out var replyId)) return false;
        bool CurrentScope() => !_chatClosing && !_savingOnClose && ActiveChatId == chat.Id &&
            FindChat(chat.Id).Chat == chat && FindChat(chat.Id).Project?.Id == task.Scope.ProjectId;
        bool HasOriginalMessages()
        {
            int position = chat.Messages.FindIndex(message => message.Id == replyId && message.Role == "assistant");
            return position >= 0 && chat.Messages.Take(position).Any(message => message.Role == "user");
        }
        if (HasOriginalMessages()) return true;
        await EnsureRecentHistoryAsync(chat);
        if (!_historyPages.TryGetValue(chat.Id, out var history)) return false;
        while (history.Loading && CurrentScope()) await Task.Delay(40, _historyLifetime.Token);
        if (!CurrentScope() || !CanResumeSelectedTask(task)) return false;
        if (!HasOriginalMessages()) await LoadHistoryAsync(history, "latest");
        while (!HasOriginalMessages() && history.HasMore && CurrentScope() && CanResumeSelectedTask(task))
        {
            string? previousCursor = history.Cursor;
            await LoadHistoryAsync(history, "older");
            if (previousCursor == history.Cursor) break;
        }
        return CurrentScope() && HasOriginalMessages();
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

    private Task<PausedTaskSelection?> GetPausedTaskRecovery(ProjectChatState chat, Guid? projectId)
    {
        if (!chat.IsPersisted) return Task.FromResult<PausedTaskSelection?>(new(null, false));
        if (_pausedTaskRecovery is { } cached && cached.ConversationId == chat.Id && cached.ProjectId == projectId
            && !(cached.Selection.IsCompletedSuccessfully && cached.Selection.Result is null)) return cached.Selection;
        // Recover once while a chat opens; ordinary sends share the result instead of querying before showing input.
        // 打开聊天时后台恢复一次；普通发送复用结果，不在显示输入前逐轮查询暂停任务。
        var selection = ReadPausedTaskRecoveryAsync(chat.Id, projectId);
        _pausedTaskRecovery = new(chat.Id, projectId, selection);
        return selection;
    }

    private async Task<PausedTaskSelection?> ReadPausedTaskRecoveryAsync(Guid conversation, Guid? projectId)
    {
        try
        {
            var response = await _tasksApiClient.PausedAsync(conversation, projectId, _historyLifetime.Token);
            var selected = PausedTaskSelection.From(response);
            var owner = FindChat(conversation);
            if (!_chatClosing && owner.Chat is not null && owner.Project?.Id == projectId && selected.TaskId is Guid paused &&
                !_pausedTaskIds.ContainsKey(conversation))
                _pausedTaskIds[conversation] = paused;
            return selected;
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException
            or InvalidOperationException or InvalidDataException or System.Text.Json.JsonException or ArgumentException)
        {
            System.Diagnostics.Debug.WriteLine($"Paused task recovery: {error.GetType().Name}");
            return null;
        }
    }

    private async Task<bool> QueueRecoveredPausedPromptAsync(PendingChatReply pending)
    {
        var (chat, project) = FindChat(pending.ConversationId);
        if (chat is null) throw new OperationCanceledException(pending.Cancellation.Token);
        var selected = await GetPausedTaskRecovery(chat, project?.Id);
        pending.Cancellation.Token.ThrowIfCancellationRequested();
        if (_chatClosing || _savingOnClose || FindChat(chat.Id).Chat != chat || FindChat(chat.Id).Project?.Id != project?.Id)
            throw new OperationCanceledException(pending.Cancellation.Token);
        if (selected is null)
            throw new InvalidOperationException(UiText.Get("暂停任务读取失败，输入内容已保留；请重试或在任务侧栏查看。"));
        if (selected.IsAmbiguous)
            throw new InvalidOperationException(UiText.Get("此聊天有多个暂停任务，请在任务侧栏选择要继续的任务；输入内容已保留。"));
        if (selected.TaskId is not Guid taskId) return false;
        _pausedTaskIds[chat.Id] = taskId;
        var user = chat.Messages.First(message => message.Id == pending.UserMessageId);
        _messageAttachmentDrafts.Remove(user.Id, out var attachments);
        var queued = new QueuedPrompt(user.Id, user.Content, pending.Provider, pending.Model, pending.PermissionMode, attachments)
            { Attachments = user.Attachments.ToList() };
        QueueFor(chat.Id).Insert(0, queued);
        chat.Messages.Remove(user);
        chat.Messages.Remove(pending.Message);
        _pendingReplies.Remove(chat.Id);
        pending.Message.Status = "interrupted";
        if (ActiveChatId == chat.Id) UpdateConversationPresentation(openAtBottom: false);
        RefreshPromptQueue();
        return true;
    }

    private void ForgetPausedTaskRecovery(Guid conversation)
    {
        if (_pausedTaskRecovery?.ConversationId == conversation) _pausedTaskRecovery = null;
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
                ForgetPausedTaskRecovery(conversation);
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
            ForgetPausedTaskRecovery(conversation);
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
            ForgetPausedTaskRecovery(chat.Id);
            StartReplyRefreshTimer();
            if (ActiveChatId == chat.Id) UpdateConversationPresentation(openAtBottom: false);
            UpdateSendButtonState();
            _ = ReceiveReplyAsync(pending, task.TaskId);
        }
        catch (Exception error) when (error is InvalidOperationException or IOException or UnauthorizedAccessException)
        { if (!_chatClosing) ShowActionFeedback(UiText.Get("任务已继续，打开原聊天可查看回复。")); }
    }
}
