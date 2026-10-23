using KYNXA_Desktop.Services;
using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private readonly TasksApiClient _tasksApiClient = new();
    private readonly HashSet<Guid> _resumingTaskConversations = [];

    private void InitializeTaskPanel()
    {
        TaskPanel.ConfigureApi(_tasksApiClient);
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
        try
        {
            if (!CanControlSelected())
            {
                feedback = UiText.Get("当前回复仍在处理，请稍后继续所选任务。");
                return;
            }
            if (!await EnsureSelectedTaskHistoryAsync(selected))
            {
                feedback = UiText.Get("原任务回复尚未加载，进度已保留，请刷新后继续。");
                return;
            }
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(60));
            var detail = await _tasksApiClient.GetAsync(conversation, selected.TaskId, selected.Scope.ProjectId, deadline.Token);
            current = detail.Task;
            // Recheck chat and live reply after history/version reads before controlling the root.
            // 历史和版本读取不阻塞发送或导航；控制根任务前重查聊天和活动回复。
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

    private async Task<bool> SendTaskSteeringAsync(Guid conversation, QueuedPrompt prompt)
    {
        if (_savingOnClose || _chatClosing) return false;
        prompt.IsTaskGuidance = true;
        _pendingReplies.TryGetValue(conversation, out var pending);
        Guid? taskId = prompt.TargetTaskId ?? (pending?.StreamRequestId == prompt.TargetRequestId ? pending?.TaskId : null);
        var (chat, project) = FindChat(conversation);
        if (chat is null) return false;
        bool CurrentScope() => !_savingOnClose && !_chatClosing && ActiveChatId == conversation &&
            FindChat(conversation).Chat == chat && FindChat(conversation).Project?.Id == project?.Id;
        if (!CurrentScope()) return false;
        // A selected task is authoritative; never borrow an unregistered reply placeholder's request ID.
        // 已选原任务才是正式目标，不能借用尚未登记的回复占位请求身份。
        prompt.TargetTaskId ??= taskId;
        ChatMessageState? previous = null, submitted = null;
        string? previousStatus = null;
        bool steeringAccepted = false;
        void ContinueAsOrdinaryMessage()
        {
            if (submitted is not null)
                chat.Messages.RemoveAll(message => message.Id == submitted.Id || message.Id == prompt.ReplyMessageId);
            if (previous is not null)
            {
                previous.SupersededByUserMessageId = null;
                previous.Status = previousStatus ?? "completed";
                if (pending is not null) { SetPendingReplyMessage(pending, previous); pending.WaitingReplyAnchorId = null; }
            }
            prompt.IsTaskGuidance = false;
            prompt.TargetTaskId = null;
            prompt.TargetRequestId = null;
        }
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(_historyLifetime.Token,
            prompt.TargetTaskId is not null && _runningTaskRecovery?.Chat.Id == conversation
                ? _runningTaskRecovery.Cancellation.Token : CancellationToken.None);
        deadline.CancelAfter(TimeSpan.FromSeconds(60));
        try
        {
            if (taskId is null && pending is not null && prompt.TargetRequestId == pending.StreamRequestId)
                taskId = await pending.TaskRegistered.Task.WaitAsync(deadline.Token);
            if (taskId is null && pending is not null && pending.Finished.Task.IsCompleted
                && pending.Message.Status == "completed" && !pending.StopRequested && !pending.PauseConfirmed)
            { ContinueAsOrdinaryMessage(); return false; }
            if (taskId is null || _chatClosing) throw new InvalidOperationException("The task has no active continuation.");
            prompt.TargetTaskId ??= taskId;
            var current = await _tasksApiClient.GetAsync(conversation, taskId.Value, project?.Id, deadline.Token);
            if (!CurrentScope()) throw new OperationCanceledException(deadline.Token);
            if (current.Task.State == "completed" && !prompt.SteeringDeliveryUnconfirmed)
            { ContinueAsOrdinaryMessage(); return false; }
            if (_pendingReplies.GetValueOrDefault(conversation) != pending)
                throw new InvalidOperationException("The active reply changed while preparing guidance.");
            if (prompt.TargetRequestId is Guid expectedRequest &&
                (!Guid.TryParse(current.Task.RequestId, out var currentRequest) || currentRequest != expectedRequest))
                throw new InvalidDataException("The recovered task request changed.");
            if (!Guid.TryParse(current.Task.RequestId, out var boundRequest) || boundRequest == Guid.Empty)
                throw new InvalidDataException("The original task request is invalid.");
            prompt.TargetRequestId ??= boundRequest;
            if (pending is not null && (pending.TaskId != taskId || pending.StreamRequestId != boundRequest))
                throw new InvalidDataException("The active reply belongs to another task request.");
            if (current.Task.State == "partial" && prompt.IsTaskGuidance)
            {
                // Resume partial work through the original task's verification gate, without replaying successes.
                // 部分交付通过原任务 resume 入口核验并继续，不新建根任务或重做成功操作。
                var resumed = await _tasksApiClient.ControlAsync(conversation, taskId.Value, current.Task.Version,
                    "resume", project?.Id, deadline.Token);
                if (!CurrentScope()) throw new OperationCanceledException(deadline.Token);
                current = current with { Task = resumed.Task };
            }
            if (TaskStates.IsTerminal(current.Task.State) && !(current.Task.State == "completed" && prompt.SteeringDeliveryUnconfirmed))
            {
                throw new InvalidOperationException("The task has already ended.");
            }
            // Bind both durable identities before replacing any visible reply or preparing an upload.
            // 正式任务与原请求身份都固定后，才替换可见回复或准备附件上传。
            previous = pending?.Message ?? FindTaskSteeringReply(chat, boundRequest);
            previousStatus = previous?.Status;
            submitted = PrepareSteeringReply(conversation, prompt, pending);
            await PrepareMessageAttachmentsAsync(conversation, submitted, deadline.Token);
            if (!CurrentScope()) throw new OperationCanceledException(deadline.Token);
            prompt.Attachments = submitted.Attachments.ToList();
            TaskControlResponse receipt;
            try
            {
                prompt.SteeringDeliveryUnconfirmed = true;
                receipt = await _tasksApiClient.SteerAsync(conversation, taskId.Value, current.Task.Version,
                    submitted.Id, prompt.Text, "continue", project?.Id, deadline.Token, replyMessageId: prompt.ReplyMessageId,
                    attachments: submitted.Attachments.ToArray());
            }
            catch (GatewayApiException error) when (error.StatusCode == System.Net.HttpStatusCode.Conflict
                && error.ErrorCode is "TASK_VERSION_CONFLICT" or "TASK_CONFLICT")
            {
                prompt.SteeringDeliveryUnconfirmed = false;
                // Refresh only a rejected version; keep the same task and message identities.
                // 仅对已明确拒绝的旧版本刷新一次，保持任务及引导消息身份，不重放未知操作。
                current = await _tasksApiClient.GetAsync(conversation, taskId.Value, project?.Id, deadline.Token);
                if (current.Task.State == "completed" && CurrentScope())
                { ContinueAsOrdinaryMessage(); return false; }
                if (TaskStates.IsTerminal(current.Task.State) || !CurrentScope()) throw;
                prompt.SteeringDeliveryUnconfirmed = true;
                receipt = await _tasksApiClient.SteerAsync(conversation, taskId.Value, current.Task.Version,
                    submitted.Id, prompt.Text, "continue", project?.Id, deadline.Token, replyMessageId: prompt.ReplyMessageId,
                    attachments: submitted.Attachments.ToArray());
            }
            steeringAccepted = true;
            prompt.SteeringDeliveryUnconfirmed = false;
            QueueFor(conversation).Remove(prompt);
            RefreshPromptQueue();
            TaskPanel.NotifyChanged(conversation);
            if (receipt.Task.State == "paused")
            {
                if (prompt.TargetTaskId is null || !prompt.IsTaskGuidance || !CurrentScope()) return true;
                // Only this explicit continuation may resume the paused root, through its normal verification gate.
                // 只有本次明确续作才恢复暂停根，仍走既有核验入口，不能绕过未知副作用检查。
                receipt = await _tasksApiClient.ControlAsync(conversation, taskId.Value, receipt.Task.Version,
                    "resume", project?.Id, deadline.Token);
            }
            if (!CurrentScope()) return true;
            if (receipt.ReconnectRequired && pending is not null)
            {
                pending.PauseConfirmed = true;
                pending.Cancellation.Cancel();
            }
            if (pending is null || receipt.ReconnectRequired)
                await ObserveResumedTaskAsync(receipt.Task, expectedReplyId: prompt.ReplyMessageId);
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
            if (submitted is not null)
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
    private async Task ObserveResumedTaskAsync(TaskRecord task, TaskCheckpoint? checkpoint = null, bool showFeedback = true,
        Guid? expectedReplyId = null)
    {
        if (_chatClosing || ActiveChatId != task.Scope.ConversationId || task.ParentTaskId is not null || task.State is not ("queued" or "running")
            || !Guid.TryParse(task.RequestId, out var requestId)) return;
        if (_pendingReplies.TryGetValue(task.Scope.ConversationId, out var prior))
        {
            if (prior.TaskId != task.TaskId || !prior.PauseConfirmed) return;
            try { await prior.Finished.Task.WaitAsync(TimeSpan.FromSeconds(15)); }
            catch (TimeoutException)
            {
                if (!_chatClosing && showFeedback) ShowActionFeedback(UiText.Get("任务已继续，打开原聊天可查看回复。"));
                return;
            }
        }
        var (chat, project) = FindChat(task.Scope.ConversationId);
        if (chat is null || ActiveChatId != chat.Id || project?.Id != task.Scope.ProjectId) return;
        var rootMessage = chat.Messages.FirstOrDefault(item => item.Id == requestId && item.Role == "assistant");
        Guid? checkpointReplyId = null;
        if (checkpoint?.ContextReference is { ValueKind: System.Text.Json.JsonValueKind.Object } context &&
            context.TryGetProperty("conversationId", out var savedConversation) && savedConversation.TryGetGuid(out var savedChat) &&
            savedChat == chat.Id && context.TryGetProperty("requestId", out var savedRequest) &&
            savedRequest.TryGetGuid(out var originalRequest) && originalRequest == requestId &&
            context.TryGetProperty("assistantMessageId", out var savedReply) && savedReply.TryGetGuid(out var replyId))
            checkpointReplyId = replyId;
        var checkpointMessage = chat.Messages.FirstOrDefault(item => item.Id == checkpointReplyId && item.Role == "assistant" &&
            item.SupersededByUserMessageId is null);
        var original = rootMessage is null ? null : chat.Messages.Take(chat.Messages.IndexOf(rootMessage)).LastOrDefault(item => item.Role == "user");
        Guid? originalUserId = checkpointMessage?.OriginUserMessageId ?? rootMessage?.OriginUserMessageId ??
            rootMessage?.InReplyToUserMessageId ?? original?.Id;
        var message = (expectedReplyId is Guid expected
            ? chat.Messages.FirstOrDefault(item => item.Id == expected && item.Role == "assistant") : null)
            ?? checkpointMessage ?? chat.Messages.LastOrDefault(item => item.Role == "assistant" &&
            item.SupersededByUserMessageId is null && (item.Id == requestId ||
                originalUserId is Guid origin && item.OriginUserMessageId == origin));
        originalUserId ??= message?.InReplyToUserMessageId;
        if (message is null || originalUserId is not Guid userId)
        {
            if (!_chatClosing && showFeedback) ShowActionFeedback(UiText.Get("任务已继续，打开原聊天可查看回复。"));
            return;
        }
        try
        {
            // Cache invalidation is ordered per message in the background; observing never waits for global disk writes.
            // 缓存失效按消息有序后台落盘；重接观察不能等待所有聊天的磁盘写入。
            _ = MessageTimePresentation.ForgetAsync(chat.Id, message);
            if (_chatClosing || _pendingReplies.ContainsKey(chat.Id)) return;
            original = chat.Messages.FirstOrDefault(item => item.Id == userId && item.Role == "user");
            var pending = new PendingChatReply(chat.Id, userId, original?.Content ?? task.Goal, message.Provider, message.Model,
                _layout.PermissionMode, message) { TaskId = task.TaskId, Started = true, StreamRequestId = requestId,
                    WaitingReplyAnchorId = expectedReplyId };
            pending.TaskRegistered.TrySetResult(task.TaskId);
            // Seed persisted timing before entering streaming; process-local clocks do not survive a restart.
            // 先接续正式累计值再进入恢复准备；重启后没有本机时钟，等待新模型启动期间也要保留已有用时。
            if (message.GenerationElapsedMs is long resumedElapsed) message.ApplyGenerationTiming(resumedElapsed, false);
            message.Status = "streaming"; message.Error = string.Empty;
            pending.Content.Append(message.Content); pending.Reasoning.Append(message.Reasoning);
            foreach (var segment in message.AssistantSegments)
                pending.SegmentText[segment.Id] = (new System.Text.StringBuilder(segment.Content), new System.Text.StringBuilder(segment.Reasoning));
            pending.Presentation = new ConversationMessageViewModel(chat.Id, message);
            _pendingReplies.Add(chat.Id, pending);
            StartReplyRefreshTimer();
            if (ActiveChatId == chat.Id) UpdateConversationPresentation(openAtBottom: false);
            UpdateSendButtonState();
            _ = ReceiveReplyAsync(pending, task.TaskId);
        }
        catch (Exception error) when (error is InvalidOperationException or IOException or UnauthorizedAccessException)
        { if (!_chatClosing && showFeedback) ShowActionFeedback(UiText.Get("任务已继续，打开原聊天可查看回复。")); }
    }
}
