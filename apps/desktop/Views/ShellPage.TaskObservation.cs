using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private sealed class RunningTaskRecovery(ProjectChatState chat, Guid? projectId, CancellationTokenSource cancellation)
    {
        public ProjectChatState Chat { get; } = chat;
        public Guid? ProjectId { get; } = projectId;
        public CancellationTokenSource Cancellation { get; } = cancellation;
        public TaskRecord[]? Tasks { get; set; }
        public TaskDetailResponse? Detail { get; set; }
        public Task? ReadTask { get; set; }
        public bool Attaching { get; set; }
        public bool Finished { get; set; }
    }

    private RunningTaskRecovery? _runningTaskRecovery;

    private bool IsCurrentTaskRecovery(RunningTaskRecovery recovery) => !_chatClosing && !_savingOnClose && !_projectViewClosed &&
        !recovery.Cancellation.IsCancellationRequested && ReferenceEquals(_runningTaskRecovery, recovery) &&
        ActiveChatId == recovery.Chat.Id && FindChat(recovery.Chat.Id).Chat == recovery.Chat &&
        FindChat(recovery.Chat.Id).Project?.Id == recovery.ProjectId;

    private void RecoverRunningTaskObservation(ProjectChatState? chat, Guid? projectId)
    {
        if (chat is null || _runningTaskRecovery is { } old &&
            (old.Chat != chat || old.ProjectId != projectId))
        {
            _runningTaskRecovery?.Cancellation.Cancel();
            _runningTaskRecovery?.Cancellation.Dispose();
            _runningTaskRecovery = null;
        }
        if (chat is not { IsPersisted: true } || _chatClosing || _savingOnClose || _projectViewClosed ||
            _pendingReplies.ContainsKey(chat.Id) || _resumingTaskConversations.Contains(chat.Id)) return;
        if (_runningTaskRecovery is null)
        {
            var recovery = new RunningTaskRecovery(chat, projectId,
                CancellationTokenSource.CreateLinkedTokenSource(_historyLifetime.Token));
            _runningTaskRecovery = recovery;
            recovery.ReadTask = ReadRunningTaskObservationAsync(recovery);
        }
        else if (_runningTaskRecovery is { Tasks: not null, Attaching: false, Finished: false } recovery)
            _ = AttachRunningTaskObservationAsync(recovery);
    }

    private async Task ReadRunningTaskObservationAsync(RunningTaskRecovery recovery)
    {
        try
        {
            var tasks = new List<TaskRecord>();
            Guid? cursor = null;
            do
            {
                var page = await _tasksApiClient.ListConversationAsync(recovery.Chat.Id, recovery.ProjectId,
                    cursor, recovery.Cancellation.Token);
                if (!IsCurrentTaskRecovery(recovery)) return;
                tasks.AddRange(page.Tasks);
                cursor = page.HasMore ? page.NextTaskId : null;
                // Multiple active roots require selection; background observation never guesses between them.
                // 多个活动根需要用户选择；后台观察不代选，也不恢复任何暂停根。
                if (tasks.Count(task => task.ParentTaskId is null && task.State is "queued" or "running") > 1) break;
            } while (cursor is not null);
            recovery.Tasks = tasks.ToArray();
            await AttachRunningTaskObservationAsync(recovery);
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException or InvalidOperationException or
            InvalidDataException or IOException or UnauthorizedAccessException or System.Text.Json.JsonException or ArgumentException)
        {
            recovery.Finished = true;
            System.Diagnostics.Debug.WriteLine($"Task observation recovery: {error.GetType().Name}");
        }
    }

    private async Task AttachRunningTaskObservationAsync(RunningTaskRecovery recovery)
    {
        if (!IsCurrentTaskRecovery(recovery) || _sendingPrompt || recovery.Attaching || recovery.Finished ||
            _pendingReplies.ContainsKey(recovery.Chat.Id) || _resumingTaskConversations.Contains(recovery.Chat.Id)) return;
        recovery.Attaching = true;
        try
        {
            var candidate = RunningTaskObservation.Select(recovery.Tasks ?? [], recovery.Chat.Id, recovery.ProjectId);
            if (candidate is null)
            {
                if ((recovery.Tasks ?? []).Any(task => task.ParentTaskId is null && task.State is "queued" or "running"))
                { recovery.Finished = true; return; }
                // Wait for the existing history load; settled chats need no extra terminal detail or refresh.
                // 等待既有历史加载回调；稳定聊天没有生成中的当前回复时，不额外读取终态详情或刷新。
                if (!_historyPages.TryGetValue(recovery.Chat.Id, out var loadedHistory) ||
                    !loadedHistory.Initialized || loadedHistory.Loading) return;
                if (!recovery.Chat.Messages.Any(message => message.Role == "assistant" &&
                    message.SupersededByUserMessageId is null && message.Status == "streaming"))
                { recovery.Finished = true; return; }
                candidate = (recovery.Tasks ?? []).Where(task => task.ParentTaskId is null && TaskStates.IsTerminal(task.State))
                    .OrderByDescending(task => task.UpdatedAtMs).FirstOrDefault();
            }
            if (candidate is null) { recovery.Finished = true; return; }
            recovery.Detail ??= await _tasksApiClient.GetAsync(recovery.Chat.Id, candidate.TaskId, recovery.ProjectId,
                recovery.Cancellation.Token);
            if (!IsCurrentTaskRecovery(recovery) || _sendingPrompt || _pendingReplies.ContainsKey(recovery.Chat.Id) ||
                _resumingTaskConversations.Contains(recovery.Chat.Id)) return;
            var current = recovery.Detail.Task;
            if (TaskStates.IsTerminal(current.State))
            {
                if (!_historyPages.TryGetValue(recovery.Chat.Id, out var history) || history.Loading) return;
                recovery.Finished = true;
                // Completion may race discovery; reload the formal reply, including partial task outcomes, without controls.
                // 发现期间任务可能已收尾；只刷新正式回复，partial也照常显示，不恢复或重做任何操作。
                await LoadHistoryAsync(history, "refresh");
                return;
            }
            if (current.State is "queued" or "running")
            {
                await ObserveResumedTaskAsync(current, recovery.Detail.Checkpoint, showFeedback: false);
                recovery.Finished = _pendingReplies.TryGetValue(recovery.Chat.Id, out var pending) && pending.TaskId == current.TaskId;
            }
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException or InvalidOperationException or
            InvalidDataException or IOException or UnauthorizedAccessException or System.Text.Json.JsonException or ArgumentException)
        {
            recovery.Finished = true;
            System.Diagnostics.Debug.WriteLine($"Task observer attachment: {error.GetType().Name}");
        }
        finally { recovery.Attaching = false; }
    }

    private async Task<bool> TryRouteRecoveredContinuationAsync(string text, string originalDraft,
        string attachmentDraftKey, MessageAttachmentDraft[] attachments, string? provider, string? model)
    {
        if (!RecoveredTaskContinuation.IsExplicit(text) || ActiveChatId is not Guid conversation ||
            FindChat(conversation).Chat is not { IsPersisted: true } chat || _pendingReplies.ContainsKey(conversation)) return false;
        Guid? projectId = FindChat(conversation).Project?.Id;
        try
        {
            // Reuse the open-chat discovery, including an in-flight read; sending never races it into another root.
            // 共用打开聊天的发现结果及正在进行的读取；发送不能与后台恢复竞争而建立另一个根任务。
            RecoverRunningTaskObservation(chat, projectId);
            var recovery = _runningTaskRecovery;
            if (recovery is null || recovery.Chat != chat || recovery.ProjectId != projectId) return true;
            if (recovery.ReadTask is { } reading) await reading;
            if (!IsCurrentTaskRecovery(recovery)) return true;
            if (recovery.Tasks is null) throw new InvalidOperationException("The original task could not be checked.");
            var selected = RecoveredTaskContinuation.Select(recovery.Tasks, conversation, projectId);
            if (selected.Task is null && !selected.IsAmbiguous && recovery.Finished)
            {
                // A settled open-chat snapshot is not proof that no task was accepted later.
                // 打开聊天时的稳定快照不能证明之后没有新任务；仅缺少活动身份的明确续作补查一次。
                recovery.Finished = false;
                recovery.Detail = null;
                recovery.Tasks = null;
                recovery.ReadTask = ReadRunningTaskObservationAsync(recovery);
                await recovery.ReadTask;
                if (!IsCurrentTaskRecovery(recovery)) return true;
                if (recovery.Tasks is null) throw new InvalidOperationException("The original task could not be checked.");
                selected = RecoveredTaskContinuation.Select(recovery.Tasks, conversation, projectId);
            }
            if (selected.IsAmbiguous)
            {
                ShowActionFeedback(UiText.Get("此聊天有多个未完成任务，请在任务侧栏选择要继续的任务；输入内容已保留。"));
                TaskPanel.NotifyChanged(conversation);
                return true;
            }
            if (selected.Task is not { } task) return false;
            if (!await EnsureSelectedTaskHistoryAsync(task) || !IsCurrentTaskRecovery(recovery))
            {
                if (IsCurrentTaskRecovery(recovery)) ShowActionFeedback(UiText.Get("原任务回复尚未加载，输入内容已保留，请刷新后继续。"));
                return true;
            }
            var prompt = new QueuedPrompt(Guid.NewGuid(), text, provider, model, _layout.PermissionMode, attachments)
                { TargetTaskId = task.TaskId, TargetRequestId = Guid.Parse(task.RequestId) };
            QueueFor(conversation).Add(prompt);
            var acceptedAttachments = attachments.Select(file => file.Id).ToHashSet();
            AttachmentDraftsFor(attachmentDraftKey).RemoveAll(file => acceptedAttachments.Contains(file.Id));
            if (PromptTextBox.Text == originalDraft) PromptTextBox.Text = ViewModel.Prompt = chat.Draft = string.Empty;
            RefreshPromptQueue();
            await SteerQueuedPromptAsync(conversation, prompt);
            return true;
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException or InvalidOperationException or
            InvalidDataException or IOException or UnauthorizedAccessException or System.Text.Json.JsonException or ArgumentException)
        {
            if (!_chatClosing && ActiveChatId == conversation && FindChat(conversation).Project?.Id == projectId)
                ShowActionFeedback(UiText.Get("原任务状态尚未确认，输入内容已保留；请重试或在任务侧栏查看。"));
            return true;
        }
    }
}
