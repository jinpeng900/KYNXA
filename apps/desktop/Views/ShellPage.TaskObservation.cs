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
        public bool ReadFailed { get; set; }
    }

    private RunningTaskRecovery? _runningTaskRecovery;

    private void ReleaseTaskReplyObservation(PendingChatReply pending)
    {
        if (_chatClosing || _savingOnClose || ActiveChatId != pending.ConversationId) return;
        if (_runningTaskRecovery is { } recovery && recovery.Chat.Id == pending.ConversationId)
        {
            if (pending.Message.Status == "completed" || pending.PauseConfirmed)
            {
                // A confirmed terminal stream or pause invalidates only this root's stale active projection.
                // 正式流结束或暂停回执只清除该根过期的活动投影，不改其他任务和正式记录。
                if (pending.TaskId is Guid taskId)
                {
                    recovery.Tasks = recovery.Tasks?.Where(task => task.TaskId != taskId).ToArray();
                    if (recovery.Detail?.Task.TaskId == taskId) recovery.Detail = null;
                }
                return;
            }
            // A departed SSE is not proof of task completion; refresh existing metadata once before admitting a new root.
            // SSE 退出不能证明任务完成；恢复现有元信息读取一次，再判断能否发送新轮。
            recovery.Finished = false; recovery.ReadFailed = false; recovery.Detail = null;
            recovery.ReadTask = ReadRunningTaskObservationAsync(recovery);
        }
        else if (pending.Message.Status != "completed" && !pending.PauseConfirmed)
        {
            var (chat, project) = FindChat(pending.ConversationId);
            RecoverRunningTaskObservation(chat, project?.Id);
        }
    }

    private bool IsCurrentTaskRecovery(RunningTaskRecovery recovery) => !_chatClosing && !_savingOnClose && !_projectViewClosed &&
        !recovery.Cancellation.IsCancellationRequested && ReferenceEquals(_runningTaskRecovery, recovery) &&
        ActiveChatId == recovery.Chat.Id && FindChat(recovery.Chat.Id).Chat == recovery.Chat &&
        FindChat(recovery.Chat.Id).Project?.Id == recovery.ProjectId;

    private void RecoverRunningTaskObservation(ProjectChatState? chat, Guid? projectId)
    {
        // A departed reply cannot replace the observer owned by the newly selected chat.
        // 离开的旧回复不能替换新选中聊天的观察；校验须早于取消旧读取。
        if (chat?.Id != ActiveChatId || chat is not null &&
            (FindChat(chat.Id).Chat != chat || FindChat(chat.Id).Project?.Id != projectId)) return;
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
                // Background observation only restores live output; it never classifies messages or resumes paused roots.
                // 后台观察只恢复活动输出，不分类消息，也不自动恢复暂停根。
                if (tasks.Count(task => task.ParentTaskId is null && task.State is "queued" or "running") > 1) break;
            } while (cursor is not null);
            recovery.Tasks = tasks.ToArray();
            await AttachRunningTaskObservationAsync(recovery);
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException or InvalidOperationException or
            InvalidDataException or IOException or UnauthorizedAccessException or System.Text.Json.JsonException or ArgumentException)
        {
            recovery.Finished = true;
            recovery.ReadFailed = true;
            System.Diagnostics.Debug.WriteLine($"Task observation recovery: {error.GetType().Name}");
        }
        finally
        {
            if (IsCurrentTaskRecovery(recovery))
            {
                RefreshPromptQueue();
                UpdateSendButtonState();
                if (recovery.ReadFailed && QueueFor(recovery.Chat.Id).Count > 0)
                    ShowActionFeedback(UiText.Get("消息已保留，请重试。"));
                else _ = DrainQueuedPromptsAsync(recovery.Chat.Id);
            }
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
                // Discovery may race completion; reload the formal reply, including partial outcomes.
                // 发现期间任务可能结束；仅刷新正式回复，照常显示 partial，不恢复或重做操作。
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

}
