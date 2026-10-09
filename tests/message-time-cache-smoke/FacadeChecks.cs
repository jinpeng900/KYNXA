using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;

internal static class FacadeChecks
{
    public static async Task RunAsync(string temporaryRoot, Action<bool, string> check)
    {
        var firstTime = new DateTimeOffset(2026, 10, 8, 12, 30, 0, TimeSpan.Zero);
        Guid chat = Guid.NewGuid(), messageId = Guid.NewGuid();
        string content = "Synthetic facade reply.";
        string hash = Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(content))).ToLowerInvariant();
        string retryRoot = Path.Combine(temporaryRoot, "Retry", "Desktop");
        MessageTimePresentation.ConfigureCache(null);
        try
        {
            MessageTimePresentation.ConfigureCache(retryRoot);
            var original = Message(messageId, content, firstTime.AddMinutes(-1));
            var retryCache = new MessageEndTimeCache(retryRoot);
            Directory.CreateDirectory(retryCache.DirectoryPath);
            Task<bool> pendingWrite, forget;
            using (var occupied = new FileStream(Path.Combine(retryCache.DirectoryPath, ".write.lock"),
                FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None))
            {
                MessageTimePresentation.RecordEnd(chat, original, firstTime);
                pendingWrite = MessageTimePresentation.SaveAsync(chat, original);
                check(!pendingWrite.IsCompleted, "facade: the previous attempt's save is still pending behind the root write lock");
                forget = MessageTimePresentation.ForgetAsync(chat, original);
                check(!forget.IsCompleted, "facade: retry invalidation waits for the previous in-flight save");
            }
            check(await forget.WaitAsync(TimeSpan.FromSeconds(6)) && await pendingWrite,
                "facade: retry invalidation finishes after releasing the previous write");
            var retryTime = firstTime.AddMinutes(2);
            MessageTimePresentation.RecordEnd(chat, original, retryTime);
            check(await MessageTimePresentation.SaveAsync(chat, original),
                "facade: an identical ID, status and content can save a fresh retry observation");
            MessageTimePresentation.ConfigureCache(null);
            MessageTimePresentation.ConfigureCache(retryRoot);
            var reloadedRetry = Reload(original);
            await MessageTimePresentation.RestoreAsync(chat, [reloadedRetry]);
            check(MessageTimePresentation.GetEnd(chat, reloadedRetry) == retryTime,
                "facade: a new message object restores the new attempt's time rather than a late old save");

            string failedForgetRoot = Path.Combine(temporaryRoot, "FailedForget", "Desktop");
            var failedForgetCache = new MessageEndTimeCache(failedForgetRoot);
            check(await failedForgetCache.WriteAsync(new(chat, messageId, "completed", hash, firstTime)),
                "facade: a prior same-content attempt exists before retry invalidation");
            MessageTimePresentation.ConfigureCache(failedForgetRoot);
            var failedForgetMessage = Message(messageId, content, firstTime.AddMinutes(-1));
            string oldPath = Path.Combine(failedForgetCache.DirectoryPath, chat.ToString("N"), messageId.ToString("N") + ".json");
            using (var undeletable = new FileStream(oldPath, FileMode.Open, FileAccess.Read, FileShare.Read))
            {
                using (var occupied = new FileStream(Path.Combine(failedForgetCache.DirectoryPath, ".write.lock"),
                    FileMode.Open, FileAccess.ReadWrite, FileShare.None))
                {
                    MessageTimePresentation.RecordEnd(chat, failedForgetMessage, firstTime);
                    pendingWrite = MessageTimePresentation.SaveAsync(chat, failedForgetMessage);
                    forget = MessageTimePresentation.ForgetAsync(chat, failedForgetMessage);
                    check(!pendingWrite.IsCompleted && !forget.IsCompleted,
                        "facade: retry starts while an old save is still blocked and its existing file cannot be deleted");
                }
                check(await pendingWrite.WaitAsync(TimeSpan.FromSeconds(6)) && !await forget.WaitAsync(TimeSpan.FromSeconds(6)),
                    "facade: the old save finishes but the protected old observation makes invalidation report failure");
            }
            MessageTimePresentation.RecordEnd(chat, failedForgetMessage, retryTime);
            check(await MessageTimePresentation.SaveAsync(chat, failedForgetMessage),
                "facade: the retained retry barrier forces replacement after failed invalidation");
            MessageTimePresentation.ConfigureCache(null);
            MessageTimePresentation.ConfigureCache(failedForgetRoot);
            var afterFailedForget = Reload(failedForgetMessage);
            await MessageTimePresentation.RestoreAsync(chat, [afterFailedForget]);
            check(MessageTimePresentation.GetEnd(chat, afterFailedForget) == retryTime,
                "facade: a late old save cannot clear the retry barrier and preserve the obsolete same-content time");

            string undoRoot = Path.Combine(temporaryRoot, "Undo", "Desktop");
            var undoCache = new MessageEndTimeCache(undoRoot);
            check(await undoCache.WriteAsync(new(chat, messageId, "completed", hash, firstTime)),
                "facade: a historical cache entry is seeded without any live message observation");
            MessageTimePresentation.ConfigureCache(undoRoot);
            var historical = Message(messageId, content, firstTime.AddMinutes(-1));
            await MessageTimePresentation.RestoreAsync(chat, [historical]);
            check(MessageTimePresentation.GetEnd(chat, historical) == firstTime,
                "facade: a restored historical message gains a conversation-scoped observation");
            check(await MessageTimePresentation.RemoveConversationAsync(chat) && await undoCache.ReadAsync(chat, messageId) is null &&
                MessageTimePresentation.GetEnd(chat, historical) == firstTime,
                "facade: deleting a conversation clears its file while retaining the live undo observation");
            check(await MessageTimePresentation.SaveAsync(chat, historical),
                "facade: undoing with the original historical object rewrites its removed cache entry");
            MessageTimePresentation.ConfigureCache(null);
            MessageTimePresentation.ConfigureCache(undoRoot);
            var afterUndo = Reload(historical);
            await MessageTimePresentation.RestoreAsync(chat, [afterUndo]);
            check(MessageTimePresentation.GetEnd(chat, afterUndo) == firstTime,
                "facade: restored undo data survives another observation reset and message reload");

            string firstRoot = Path.Combine(temporaryRoot, "FirstRoot", "Desktop");
            string secondRoot = Path.Combine(temporaryRoot, "SecondRoot", "Desktop");
            var firstCache = new MessageEndTimeCache(firstRoot);
            var secondCache = new MessageEndTimeCache(secondRoot);
            var secondTime = firstTime.AddHours(1);
            check(await firstCache.WriteAsync(new(chat, messageId, "completed", hash, firstTime)) &&
                await secondCache.WriteAsync(new(chat, messageId, "completed", hash, secondTime)),
                "facade: independent roots hold different observations for the same public message");
            MessageTimePresentation.ConfigureCache(firstRoot);
            var switched = Message(messageId, content, firstTime.AddMinutes(-1));
            Task previousRootRestore = MessageTimePresentation.RestoreAsync(chat, [switched]);
            MessageTimePresentation.ConfigureCache(secondRoot);
            await previousRootRestore;
            check(MessageTimePresentation.GetEnd(chat, switched) is null,
                "facade: switching roots discards previous observations and any late restore from that root");
            await MessageTimePresentation.RestoreAsync(chat, [switched]);
            check(MessageTimePresentation.GetEnd(chat, switched) == secondTime,
                "facade: the switched message restores only the active root's observation");

            string failedRoot = Path.Combine(temporaryRoot, "BlockedDesktop");
            Directory.CreateDirectory(temporaryRoot);
            await File.WriteAllTextAsync(failedRoot, "Synthetic file preventing cache directory creation.");
            MessageTimePresentation.ConfigureCache(failedRoot);
            var failedMessage = Message(messageId, content, firstTime.AddMinutes(-1));
            MessageTimePresentation.RecordEnd(chat, failedMessage, firstTime);
            check(!await MessageTimePresentation.SaveAsync(chat, failedMessage) &&
                MessageTimePresentation.GetEnd(chat, failedMessage) == firstTime,
                "facade: a filesystem save failure preserves the in-memory receipt time");

            string clockRoot = Path.Combine(temporaryRoot, "DifferentCreationClock", "Desktop");
            MessageTimePresentation.ConfigureCache(clockRoot);
            var localMessage = Message(messageId, content, firstTime.AddMinutes(-1));
            MessageTimePresentation.RecordEnd(chat, localMessage, firstTime);
            check(await MessageTimePresentation.SaveAsync(chat, localMessage),
                "facade: the original local observation has a normal creation and receipt ordering");
            var serverMessage = Reload(localMessage);
            serverMessage.CreatedAt = firstTime.AddMinutes(3);
            MessageTimePresentation.ConfigureCache(null);
            MessageTimePresentation.ConfigureCache(clockRoot);
            await MessageTimePresentation.RestoreAsync(chat, [serverMessage]);
            check(MessageTimePresentation.GetEnd(chat, serverMessage) == firstTime && serverMessage.CreatedAt > firstTime,
                "facade: a later persisted gateway creation time does not invalidate an otherwise matching local observation");
            using var formal = JsonDocument.Parse(JsonSerializer.Serialize(serverMessage));
            check(!formal.RootElement.EnumerateObject().Any(property => property.Name.Contains("EndedAt", StringComparison.OrdinalIgnoreCase)),
                "facade: official message serialization still excludes the local end-time metadata");
        }
        finally
        {
            await MessageTimePresentation.FlushAsync();
            MessageTimePresentation.ConfigureCache(null);
        }
    }

    private static ChatMessageState Message(Guid messageId, string content, DateTimeOffset createdAt) => new()
    {
        Id = messageId, Role = "assistant", Status = "completed", Content = content, CreatedAt = createdAt
    };

    // Round-trip through the product converter so restored cases use fresh objects from persisted messages.
    // 使用产品转换器序列化往返，确保恢复场景使用持久化消息产生的新对象。
    private static ChatMessageState Reload(ChatMessageState message) =>
        JsonSerializer.Deserialize<ChatMessageState>(JsonSerializer.Serialize(message))
        ?? throw new InvalidOperationException("The synthetic message did not deserialize.");
}
