using System.Runtime.CompilerServices;
using System.Security.Cryptography;
using System.Text;
using KYNXA_Desktop.Models.UI;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Keeps local receipt times in a separate UI cache without changing authoritative message fields.
/// 将本机接收结束时间保存在独立界面缓存中，不修改正式消息字段。
/// </summary>
public static class MessageTimePresentation
{
    private sealed class EndObservation
    {
        public DateTimeOffset? Time { get; set; }
        public Guid? ConversationId { get; set; }
        public string? SavedSignature { get; set; }
    }

    private static readonly object Sync = new();
    private static ConditionalWeakTable<ChatMessageState, EndObservation> Observations = new();
    private static MessageEndTimeCache? Cache;
    private static readonly HashSet<Task<bool>> PendingWrites = [];
    private static readonly HashSet<(Guid ConversationId, Guid MessageId)> RetriedMessages = [];

    public static void ConfigureCache(string? desktopDirectory)
    {
        var cache = desktopDirectory is null ? null : new MessageEndTimeCache(desktopDirectory);
        lock (Sync)
        {
            if (cache is not null && Cache is not null &&
                string.Equals(cache.DirectoryPath, Cache.DirectoryPath, StringComparison.OrdinalIgnoreCase)) return;
            Cache = cache;
            Observations = new();
            RetriedMessages.Clear();
        }
    }

    public static void RecordEnd(ChatMessageState message, DateTimeOffset endedAt)
        => RecordEndCore(null, message, endedAt);

    public static void RecordEnd(Guid conversationId, ChatMessageState message, DateTimeOffset endedAt)
        => RecordEndCore(conversationId, message, endedAt);

    private static void RecordEndCore(Guid? conversationId, ChatMessageState message, DateTimeOffset endedAt)
    {
        if (!IsTerminal(message) || endedAt <= DateTimeOffset.UnixEpoch || endedAt < message.CreatedAt) return;
        // Preserve the first observed terminal instant. A late finally block must not move it forward.
        // 保留首次观测终态的时刻，迟到的 finally 不能将其向后移动。
        lock (Sync)
        {
            var observation = Observations.GetValue(message, _ => new EndObservation { ConversationId = conversationId });
            if (observation.ConversationId != conversationId && observation.ConversationId is not null) return;
            observation.Time ??= endedAt.ToUniversalTime();
        }
    }

    public static DateTimeOffset? GetEnd(ChatMessageState message) => GetEndCore(null, message);

    public static DateTimeOffset? GetEnd(Guid conversationId, ChatMessageState message) => GetEndCore(conversationId, message);

    private static DateTimeOffset? GetEndCore(Guid? conversationId, ChatMessageState message)
    {
        lock (Sync)
        {
            return IsTerminal(message) && Observations.TryGetValue(message, out var observation) &&
                (conversationId is null || observation.ConversationId is null || observation.ConversationId == conversationId)
                ? observation.Time : null;
        }
    }

    public static Task<bool> SaveAsync(Guid conversationId, ChatMessageState message)
    {
        lock (Sync)
        {
            if (Cache is null || conversationId == Guid.Empty || message.Id == Guid.Empty ||
                GetEndCore(conversationId, message) is not DateTimeOffset endedAt) return Task.FromResult(true);
            string hash = ContentHash(message.Content);
            string signature = message.Status + ":" + hash;
            var observation = Observations.GetValue(message, _ => new EndObservation());
            observation.ConversationId ??= conversationId;
            if (observation.SavedSignature == signature) return Task.FromResult(true);
            var entry = new MessageEndTimeEntry(conversationId, message.Id, message.Status, hash, endedAt);
            var cache = Cache;
            var observations = Observations;
            bool replace = RetriedMessages.Contains((conversationId, message.Id));
            Task<bool> write = SaveCoreAsync(cache, observations, message, entry, signature, replace);
            PendingWrites.Add(write);
            _ = write.ContinueWith(completed => { lock (Sync) PendingWrites.Remove(completed); }, TaskScheduler.Default);
            return write;
        }
    }

    private static async Task<bool> SaveCoreAsync(MessageEndTimeCache cache,
        ConditionalWeakTable<ChatMessageState, EndObservation> observations, ChatMessageState message,
        MessageEndTimeEntry entry, string signature, bool replace)
    {
        bool saved = await cache.WriteAsync(entry, replace);
        lock (Sync)
        {
            if (saved && ReferenceEquals(cache, Cache) && ReferenceEquals(observations, Observations))
            {
                observations.GetValue(message, _ => new EndObservation()).SavedSignature = signature;
                RetriedMessages.Remove((entry.ConversationId, entry.MessageId));
            }
        }
        return saved;
    }

    public static async Task RestoreAsync(Guid conversationId, IReadOnlyList<ChatMessageState> messages,
        CancellationToken cancellationToken = default)
    {
        MessageEndTimeCache? cache;
        ConditionalWeakTable<ChatMessageState, EndObservation> observations;
        lock (Sync) { cache = Cache; observations = Observations; }
        if (cache is null || conversationId == Guid.Empty) return;
        var candidates = messages.Where(message => IsTerminal(message) && message.Id != Guid.Empty &&
            GetEnd(conversationId, message) is null).Select(message =>
                (Message: message, Id: message.Id, Status: message.Status, Content: message.Content)).ToArray();
        // Limit concurrent file reads; late loads cannot attach a time to another attempt or data root.
        // 限制同时读取的文件数；迟到的读取不能把时间挂到另一轮回复或数据目录。
        foreach (var batch in candidates.Chunk(16))
        {
            if (cancellationToken.IsCancellationRequested) return;
            var entries = await Task.WhenAll(batch.Select(candidate => cache.ReadAsync(conversationId, candidate.Message.Id)));
            lock (Sync)
            {
                if (cancellationToken.IsCancellationRequested || !ReferenceEquals(cache, Cache) || !ReferenceEquals(observations, Observations)) return;
                for (int index = 0; index < batch.Length; index++)
                {
                    var candidate = batch[index];
                    var message = candidate.Message;
                    var entry = entries[index];
                    if (entry is null || RetriedMessages.Contains((conversationId, message.Id)) || !IsTerminal(message) ||
                        candidate.Id != message.Id || entry.MessageId != message.Id ||
                        candidate.Status != message.Status || candidate.Content != message.Content ||
                        entry.Status != message.Status || entry.ContentHash != ContentHash(message.Content)) continue;
                    var observation = observations.GetValue(message, _ => new EndObservation { ConversationId = conversationId });
                    if (observation.ConversationId is not null && observation.ConversationId != conversationId) continue;
                    observation.Time ??= entry.EndedAt;
                    observation.SavedSignature = message.Status + ":" + entry.ContentHash;
                }
            }
        }
    }

    public static async Task<bool> ForgetAsync(Guid conversationId, ChatMessageState message)
    {
        MessageEndTimeCache? cache;
        await FlushAsync();
        lock (Sync)
        {
            cache = Cache;
            Observations.Remove(message);
            RetriedMessages.Add((conversationId, message.Id));
        }
        return cache is null || await cache.RemoveAsync(conversationId, message.Id);
    }

    public static async Task<bool> RemoveConversationAsync(Guid conversationId)
    {
        MessageEndTimeCache? cache;
        lock (Sync)
        {
            cache = Cache;
            RetriedMessages.RemoveWhere(key => key.ConversationId == conversationId);
        }
        await FlushAsync();
        // Keep live object observations so the existing undo action can restore its display cache.
        // 保留仍存活对象的观测值，以便现有撤销操作恢复显示缓存。
        bool removed = cache is null || await cache.RemoveAsync(conversationId);
        lock (Sync)
        {
            if (ReferenceEquals(cache, Cache))
                foreach (var pair in Observations)
                    if (pair.Value.ConversationId == conversationId) pair.Value.SavedSignature = null;
        }
        return removed;
    }

    public static async Task FlushAsync()
    {
        while (true)
        {
            Task<bool>[] writes;
            lock (Sync) { writes = PendingWrites.Where(write => !write.IsCompleted).ToArray(); }
            if (writes.Length == 0) return;
            await Task.WhenAll(writes);
        }
    }

    // Client and gateway creation instants can differ; match public final content and status, not their clocks.
    // 客户端与网关创建时刻可能不同；按最终公开正文和状态匹配，不要求两端时钟相同。
    private static string ContentHash(string content) => Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(content))).ToLowerInvariant();

    private static bool IsTerminal(ChatMessageState message) => message.Role == "assistant" &&
        message.Status is "completed" or "error" or "interrupted";
}
