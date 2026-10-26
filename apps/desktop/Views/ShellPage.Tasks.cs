using KYNXA_Desktop.Services;
using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.ViewModels;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private readonly TasksApiClient _tasksApiClient = new();

    private async Task<TaskDetailResponse?> FindReplyTaskAsync(ProjectChatState chat, Guid? projectId,
        ChatMessageState? reply, CancellationToken cancellationToken)
    {
        Guid? cursor = null;
        var candidates = new List<TaskRecord>();
        do
        {
            var page = await _tasksApiClient.RecoverableAsync(chat.Id, projectId, cursor, cancellationToken);
            candidates.AddRange(page.Tasks);
            cursor = page.HasMore ? page.NextTaskId : null;
        } while (cursor is not null);
        TaskRecord[] matched = reply is null ? [] : candidates.Where(task => Guid.TryParse(task.RequestId, out var requestId)
            && (requestId == reply.Id || FindTaskSteeringReply(chat, requestId)?.Id == reply.Id)).ToArray();
        if (matched.Length == 1)
            return await _tasksApiClient.GetAsync(chat.Id, matched[0].TaskId, projectId, cancellationToken);
        if (reply is null && candidates.Count == 1)
            return await _tasksApiClient.GetAsync(chat.Id, candidates[0].TaskId, projectId, cancellationToken);
        // A later reply may be outside the loaded history window; match its saved checkpoint, never the latest task.
        // 原回复可能不在已加载历史页；按保存的回复检查点匹配，不能默认选择最新任务。
        TaskDetailResponse? found = null;
        if (reply is not null)
            foreach (var task in candidates)
            {
                var detail = await _tasksApiClient.GetAsync(chat.Id, task.TaskId, projectId, cancellationToken);
                if (detail.Checkpoint?.ContextReference is not { ValueKind: System.Text.Json.JsonValueKind.Object } context
                    || !context.TryGetProperty("assistantMessageId", out var identity) || !identity.TryGetGuid(out var savedReply)
                    || savedReply != reply.Id) continue;
                if (found is not null) return null;
                found = detail;
            }
        return found;
    }

    private async Task<bool> TryContinueSavedTaskAsync(string text)
    {
        // Explicit continuation is authorization; ordinary questions keep their normal send path.
        // 明确续作才授权恢复；普通新问题保持原发送流程。
        if (!System.Text.RegularExpressions.Regex.IsMatch(text,
            @"(?ix)^\s*(?:请|麻烦|帮我|那就|好的?|please|okay|ok)?\s*(?:(?:继续|接着|续上|重试)(?:\s*[吧啊呀，,。.!！]?\s*$|\s*(?:刚才|上次|之前|原来|当前|这个|上述|剩余|未完成|精修|完善|推进|执行|做|工作|优化|修改|建模|修复|测试|验证|完成))|(?:continue|resume|retry|carry\s+on)(?:\s*[.!]?\s*$|\s+(?:the\s+)?(?:previous|unfinished|remaining|same|current|work|task|refin|build|implement|test|fix)))")) return false;
        if (ActiveChatId is not Guid conversation || IsReplyInProgress(conversation)) return false;
        var (chat, project) = FindChat(conversation);
        if (chat is null) return false;
        _sendingPrompt = true; UpdateSendButtonState();
        try
        {
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(60));
            var reply = chat.Messages.LastOrDefault(message => message.Role == "assistant" && message.SupersededByUserMessageId is null);
            var detail = await FindReplyTaskAsync(chat, project?.Id, reply, deadline.Token);
            if (detail is null) return false;
            if (_chatClosing || ActiveChatId != conversation || FindChat(conversation).Chat != chat) return true;
            var prompt = new QueuedPrompt(Guid.NewGuid(), text, _selectedModel?.ProviderId, _selectedModel?.ModelId,
                _layout.PermissionMode, TakeAttachmentDrafts()) { IsTaskGuidance = true, IsSubmitting = true,
                TargetTaskId = detail.Task.TaskId, TargetRequestId = Guid.Parse(detail.Task.RequestId) };
            QueueFor(conversation).Add(prompt);
            PromptTextBox.Text = ViewModel.Prompt = chat.Draft = string.Empty;
            await SendTaskSteeringAsync(conversation, prompt);
            prompt.IsSubmitting = false;
            return true;
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException or InvalidOperationException
            or InvalidDataException or System.Text.Json.JsonException)
        { if (!_chatClosing) ShowActionFeedback(UiText.Get("消息已保留，请重试。")); return true; }
        finally { _sendingPrompt = false; RefreshPromptQueue(); UpdateSendButtonState(); }
    }

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
            if (TaskStates.IsTerminal(current.Task.State) && !(current.Task.State == "partial" && prompt.IsTaskGuidance)
                && !(current.Task.State == "completed" && prompt.SteeringDeliveryUnconfirmed))
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
            ConsumeAcceptedGuidance(conversation, prompt);
            TaskPanel.NotifyChanged(conversation);
            if (receipt.Task.State is "paused" or "waiting-config" or "waiting-network" or "partial")
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
            pending.Presentation.SetRetryAllowed(true);
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
