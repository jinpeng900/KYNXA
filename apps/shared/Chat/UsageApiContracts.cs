namespace KYNXA.Contracts;

/// <summary>
/// Read-only provider usage for one saved conversation, derived from its formal model reservations.
/// 从正式模型预约派生的已保存对话只读用量；未知计量和未结算预约不伪装成精确零。
/// </summary>
public sealed record ConversationUsageSnapshot(string Scope, Guid ConversationId, Guid? ProjectId,
    ulong ModelRequests, ulong InputTokens, ulong OutputTokens, ulong KnownTokenRequests,
    ulong UnknownTokenRequests, ulong PendingRequests, ulong CacheReadTokens, ulong CacheInputTokens,
    ulong KnownCacheRequests, double? CacheHitRate, long UpdatedAtMs);
