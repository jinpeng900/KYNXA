using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Validate the scope of an explicit unknown-outcome recovery without selecting an execution target.
/// 核验用户明确确认未知结果时的恢复范围，不代选执行目标。
/// </summary>
internal static class PausedTaskSelection
{
    internal static bool CanResumeInContext(TaskRecord task, Guid? conversationId, Guid? projectId,
        bool hasActiveReply, Guid? activeTaskId, bool activeReplyPaused) =>
        task.ParentTaskId is null && task.Scope.ConversationId == conversationId && task.Scope.ProjectId == projectId &&
        (!hasActiveReply || activeReplyPaused && activeTaskId == task.TaskId);

}
/// <summary>
/// Observe a unique active root in this chat without resuming paused work.
/// 仅观察本聊天唯一的活动根任务，不恢复暂停任务或代选其他聊天。
/// </summary>
internal static class RunningTaskObservation
{
    internal static TaskRecord? Select(IEnumerable<TaskRecord> tasks, Guid conversationId, Guid? projectId)
    {
        var candidates = tasks.Where(task => task.ParentTaskId is null && task.Scope.ConversationId == conversationId &&
            task.Scope.ProjectId == projectId && (task.State is "queued" or "running") &&
            Guid.TryParse(task.RequestId, out var requestId) && requestId != Guid.Empty).Take(2).ToArray();
        return candidates.Length == 1 ? candidates[0] : null;
    }
}
