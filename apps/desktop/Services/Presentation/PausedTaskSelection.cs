using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Selects a verified paused identity after restart without inventing a default among multiple tasks.
/// 重启后从已校验的暂停任务选择唯一身份；多项任务不能默认猜选。
/// </summary>
internal sealed record PausedTaskSelection(Guid? TaskId, bool IsAmbiguous)
{
    internal static bool CanResumeInContext(TaskRecord task, Guid? conversationId, Guid? projectId,
        bool hasActiveReply, Guid? activeTaskId, bool activeReplyPaused) =>
        task.ParentTaskId is null && task.Scope.ConversationId == conversationId && task.Scope.ProjectId == projectId &&
        (!hasActiveReply || activeReplyPaused && activeTaskId == task.TaskId);

    internal static PausedTaskSelection From(TaskPausedResponse response) => response.Tasks.Length switch
    {
        0 => new(null, false),
        1 => new(response.Tasks[0].TaskId, false),
        _ => new(null, true)
    };
}

/// <summary>
/// Reattaches only one existing active root; observing cannot resume paused work or choose another chat's task.
/// 只重新观察唯一的已有活动根任务；观察不能恢复暂停工作，也不能代选其他聊天的任务。
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

/// <summary>
/// Routes explicit continuation to one existing scoped root; ordinary questions never inherit work by guessed similarity.
/// 仅将明确续作路由到唯一同范围根任务；普通问题不按猜测的语义相似度继承旧工作。
/// </summary>
internal sealed record RecoveredTaskContinuation(TaskRecord? Task, bool IsAmbiguous)
{
    private static readonly System.Text.RegularExpressions.Regex Continuation = new(
        @"^\s*(?:请|麻烦|帮我|那就|好的|好|please|okay|ok)?\s*(?:(?:继续|接着|续上)(?:$|[\s吧啊呀，,。.!！]|刚才|上次|之前|前面|原来|当前|这个|上述|剩余|未完成|精修|完善|推进|执行|做|工作|优化|修改|建模|修复|测试|验证|完成)|(?:continue|resume|carry\s+on)(?:\s*[.!]?\s*$|\s+(?:the\s+)?(?:previous|unfinished|remaining|same|current|work|task|refin|build|implement|test|fix)))",
        System.Text.RegularExpressions.RegexOptions.IgnoreCase | System.Text.RegularExpressions.RegexOptions.CultureInvariant |
        System.Text.RegularExpressions.RegexOptions.NonBacktracking);

    internal static bool IsExplicit(string text) => Continuation.IsMatch(text);

    internal static RecoveredTaskContinuation Select(IEnumerable<TaskRecord> tasks, Guid conversationId, Guid? projectId)
    {
        var scoped = tasks.Where(task => task.ParentTaskId is null && task.Scope.ConversationId == conversationId &&
            task.Scope.ProjectId == projectId && Guid.TryParse(task.RequestId, out var requestId) && requestId != Guid.Empty).ToArray();
        var candidates = scoped.Where(task => !TaskStates.IsTerminal(task.State)).Take(2).ToArray();
        // Prefer actual unfinished execution; only without it may an explicit continuation select partial work.
        // 优先接续实际未结束执行；没有活动根时，明确续作才选择可恢复的部分成果，多个候选仍须选择。
        if (candidates.Length == 0) candidates = scoped.Where(task => task.State == "partial").Take(2).ToArray();
        return candidates.Length switch { 0 => new(null, false), 1 => new(candidates[0], false), _ => new(null, true) };
    }
}
