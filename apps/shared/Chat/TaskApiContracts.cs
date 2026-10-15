using System.Text.Json;

namespace KYNXA.Contracts;

/// <summary>
/// Durable work identity and read-only projections; attempts are separate from the task ID.
/// 持久工作身份及只读投影；执行尝试不替换任务 ID。
/// </summary>
public sealed record TaskScope(Guid ConversationId, Guid? ProjectId);
public sealed record TaskUsage(ulong ModelRequests, ulong FailedRequests, ulong Retries, ulong Compactions,
    ulong InputTokens, ulong OutputTokens, ulong UnknownTokenRequests, ulong ModelDurationMs = 0, ulong ToolDurationMs = 0);
public sealed record TaskBudget(ulong MaxModelRequests, ulong? MaxKnownTokens, ulong? MaxExecutionMs = null);
// Child request deduplication keys are namespaced strings; only task identity is always a UUID.
// 子任务请求查重键使用命名空间字符串；任务身份始终为 UUID。
public sealed record TaskRecord(Guid TaskId, string RequestId, string Goal, string[] Acceptance, TaskScope Scope,
    Guid? ParentTaskId, Guid[] DependencyIds, TaskBudget Budget, TaskUsage Usage, string State,
    ulong Version, ulong ExecutionVersion, Guid? CurrentAttemptId, ulong EventSequence, string? StopReason,
    long CreatedAtMs, long UpdatedAtMs, bool? DisplayInTaskList = null, string? Title = null);
public sealed record TaskListResponse(TaskRecord[] Tasks, bool HasMore, Guid? NextTaskId);
// A bounded same-chat lookup distinguishes no checkpoint, one checkpoint and ambiguous work.
// 同聊天有界查询区分无断点、唯一断点和多项待继续任务，不改变工作任务列表范围。
public sealed record TaskPausedResponse(TaskRecord[] Tasks);
public sealed record TaskArtifact(string ArtifactId, string Kind, JsonElement Reference,
    string Sha256, ulong SizeBytes, string? SourceVersion);
public sealed record TaskOperationIntent(string OperationId, string ToolCallId, string ToolName, string Effect);
public sealed record TaskOperation(Guid TaskId, Guid AttemptId, TaskOperationIntent Intent, string State,
    JsonElement? ResultReference, JsonElement? VerificationReference, long UpdatedAtMs);
public sealed record TaskCheckpoint(string CheckpointId, ulong ThroughEventSequence,
    JsonElement ContextReference, JsonElement NextStep, JsonElement[] SourceVersions);
public sealed record TaskDetailResponse(TaskRecord Task, TaskRecord[] Children, TaskOperation[] Operations,
    TaskCheckpoint? Checkpoint, TaskArtifact[] Artifacts);
public sealed record TaskEvent(Guid TaskId, ulong Sequence, string EventId, string Kind,
    JsonElement Payload, long CreatedAtMs);
public sealed record TaskEventsResponse(Guid TaskId, TaskEvent[] Events, ulong ThroughSequence, bool HasMore);
public sealed record TaskControlRequest(Guid ConversationId, ulong ExpectedVersion, string Action);
public sealed record TaskControlResponse(TaskRecord Task, Guid[] AffectedTaskIds, bool ReconnectRequired = false,
    Guid? ReplyMessageId = null);
public sealed record TaskSteeringRequest(Guid ConversationId, ulong ExpectedVersion, Guid UserMessageId,
    string Message, string Action, Guid? ReplyMessageId = null, ChatAttachment[]? Attachments = null);

public static class TaskStates
{
    public static bool IsSupported(string? state) => state is "queued" or "running" or "waiting-network"
        or "waiting-config" or "waiting-approval" or "waiting-verification" or "waiting-children"
        or "paused" or "cancelled" or "failed" or "completed" or "partial";
    public static bool IsTerminal(string state) => state is "cancelled" or "failed" or "completed" or "partial";
    public static bool CanResume(string state) => state is "paused" or "waiting-network" or "waiting-config"
        or "waiting-approval" or "waiting-verification" or "waiting-children";
}
