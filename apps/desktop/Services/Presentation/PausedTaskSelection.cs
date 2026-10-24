using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

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
