using System.Runtime.CompilerServices;
using KYNXA_Desktop.Models.UI;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Keeps local receipt times as transient presentation metadata, never as authoritative stored message fields.
/// 将本机接收结束时间保留为临时展示元数据，不写成正式消息存储字段。
/// </summary>
public static class MessageTimePresentation
{
    private sealed class EndObservation
    {
        public DateTimeOffset? Time { get; set; }
    }

    private static readonly ConditionalWeakTable<ChatMessageState, EndObservation> Observations = new();

    public static void RecordEnd(ChatMessageState message, DateTimeOffset endedAt)
    {
        if (!IsTerminal(message) || endedAt <= DateTimeOffset.UnixEpoch || endedAt < message.CreatedAt) return;
        // Preserve the first observed terminal instant. A late finally block must not move it forward.
        // 保留首次观测终态的时刻，迟到的 finally 不能将其向后移动。
        Observations.GetValue(message, _ => new EndObservation()).Time ??= endedAt;
    }

    public static DateTimeOffset? GetEnd(ChatMessageState message) =>
        IsTerminal(message) && Observations.TryGetValue(message, out var observation) ? observation.Time : null;

    private static bool IsTerminal(ChatMessageState message) => message.Role == "assistant" &&
        message.Status is "completed" or "error" or "interrupted";
}
