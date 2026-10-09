using System.Diagnostics;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace KYNXA_Desktop.Services;

public sealed record MessageEndTimeEntry(Guid ConversationId, Guid MessageId, string Status,
    string ContentHash, DateTimeOffset EndedAt);

/// <summary>
/// Stores only local terminal observations; the gateway remains the owner of conversation messages.
/// 仅保存本机终态观测时间；正式聊天消息仍由网关拥有。
/// </summary>
public sealed class MessageEndTimeCache
{
    private const int SchemaVersion = 1;
    private const int MaximumFileBytes = 16 * 1024;
    private static readonly TimeSpan LockTimeout = TimeSpan.FromSeconds(2);
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web)
    {
        UnmappedMemberHandling = JsonUnmappedMemberHandling.Disallow
    };

    private sealed record StoredEntry(int SchemaVersion, Guid ConversationId, Guid MessageId, string Status,
        string ContentHash, DateTimeOffset EndedAt);
    private sealed record ReadResult(MessageEndTimeEntry? Entry, bool IsMissing);

    public string DirectoryPath { get; }

    public MessageEndTimeCache(string desktopDirectory)
    {
        if (string.IsNullOrWhiteSpace(desktopDirectory) || !Path.IsPathFullyQualified(desktopDirectory))
            throw new ArgumentException("An absolute desktop directory is required.", nameof(desktopDirectory));
        DirectoryPath = Path.Combine(Path.GetFullPath(desktopDirectory), "MessageTimes");
    }

    public async Task<MessageEndTimeEntry?> ReadAsync(Guid conversationId, Guid messageId)
    {
        if (conversationId == Guid.Empty || messageId == Guid.Empty) return null;
        return (await ReadFileAsync(FilePath(conversationId, messageId), conversationId, messageId).ConfigureAwait(false)).Entry;
    }

    public async Task<bool> WriteAsync(MessageEndTimeEntry entry, bool replaceExisting = false)
    {
        if (!IsValid(entry)) return false;
        string? temporaryPath = null;
        try
        {
            await using var writeLock = await AcquireWriteLockAsync().ConfigureAwait(false);
            if (writeLock is null) return false;
            string path = FilePath(entry.ConversationId, entry.MessageId);
            var previous = await ReadFileAsync(path, entry.ConversationId, entry.MessageId).ConfigureAwait(false);
            // Unknown or damaged records remain untouched, including when a retry requests replacement.
            // 未知版本或损坏记录始终原样保留，明确重试也不能覆盖。
            if (!previous.IsMissing && previous.Entry is null) return false;
            if (!replaceExisting && previous.Entry is { } existing &&
                existing.Status == entry.Status && existing.ContentHash == entry.ContentHash) return true;

            string folder = Path.GetDirectoryName(path)!;
            Directory.CreateDirectory(folder);
            temporaryPath = Path.Combine(folder, $".{entry.MessageId:N}.{Guid.NewGuid():N}.tmp");
            var stored = new StoredEntry(SchemaVersion, entry.ConversationId, entry.MessageId, entry.Status,
                entry.ContentHash, entry.EndedAt.ToUniversalTime());
            await using (var output = new FileStream(temporaryPath, FileMode.CreateNew, FileAccess.Write, FileShare.None,
                4096, FileOptions.Asynchronous | FileOptions.WriteThrough))
            {
                await JsonSerializer.SerializeAsync(output, stored, JsonOptions).ConfigureAwait(false);
                await output.FlushAsync().ConfigureAwait(false);
            }
            // A unique sibling and one root lock prevent partial reads and concurrent first-observation loss.
            // 同目录唯一临时文件与根级写锁避免读取半条记录，并保留并发写入中的首次观测。
            File.Move(temporaryPath, path, overwrite: true);
            temporaryPath = null;
            return true;
        }
        catch (Exception error) when (IsCacheFailure(error))
        {
            Report("write", error.GetType().Name);
            return false;
        }
        finally
        {
            if (temporaryPath is not null)
            {
                try { File.Delete(temporaryPath); }
                catch (Exception error) when (IsCacheFailure(error)) { Report("temporary cleanup", error.GetType().Name); }
            }
        }
    }

    public async Task<bool> RemoveAsync(Guid conversationId, Guid? messageId = null)
    {
        if (conversationId == Guid.Empty || messageId == Guid.Empty) return false;
        try
        {
            string folder = Path.Combine(DirectoryPath, conversationId.ToString("N"));
            if (!Directory.Exists(folder)) return true;
            await using var writeLock = await AcquireWriteLockAsync().ConfigureAwait(false);
            if (writeLock is null) return false;
            if (messageId is { } singleMessageId)
                return await RemoveFileAsync(FilePath(conversationId, singleMessageId), conversationId, singleMessageId).ConfigureAwait(false);

            bool removed = true;
            // Only validated GUID-named message files belong to this cache; never recurse or delete other files.
            // 缓存仅删除已验证的消息 GUID 文件，不递归删除目录或处理其他文件。
            foreach (string path in Directory.EnumerateFiles(folder, "*.json", SearchOption.TopDirectoryOnly))
            {
                if (!Guid.TryParseExact(Path.GetFileNameWithoutExtension(path), "N", out Guid identifier) || identifier == Guid.Empty)
                    continue;
                removed &= await RemoveFileAsync(path, conversationId, identifier).ConfigureAwait(false);
            }
            return removed;
        }
        catch (Exception error) when (IsCacheFailure(error))
        {
            Report("remove", error.GetType().Name);
            return false;
        }
    }

    private async Task<bool> RemoveFileAsync(string path, Guid conversationId, Guid messageId)
    {
        var previous = await ReadFileAsync(path, conversationId, messageId).ConfigureAwait(false);
        if (previous.IsMissing) return true;
        if (previous.Entry is null) return false;
        File.Delete(path);
        return true;
    }

    private async Task<ReadResult> ReadFileAsync(string path, Guid conversationId, Guid messageId)
    {
        try
        {
            await using var input = new FileStream(path, FileMode.Open, FileAccess.Read,
                FileShare.ReadWrite | FileShare.Delete, 4096, FileOptions.Asynchronous | FileOptions.SequentialScan);
            if (input.Length > MaximumFileBytes)
            {
                Report("read", "size limit");
                return new(null, false);
            }
            byte[] bytes = new byte[MaximumFileBytes + 1];
            int length = 0;
            while (length < bytes.Length)
            {
                int count = await input.ReadAsync(bytes.AsMemory(length)).ConfigureAwait(false);
                if (count == 0) break;
                length += count;
            }
            if (length > MaximumFileBytes)
            {
                Report("read", "size limit");
                return new(null, false);
            }
            var stored = JsonSerializer.Deserialize<StoredEntry>(bytes.AsSpan(0, length), JsonOptions);
            var entry = stored is null ? null : new MessageEndTimeEntry(stored.ConversationId, stored.MessageId,
                stored.Status, stored.ContentHash, stored.EndedAt);
            if (stored?.SchemaVersion != SchemaVersion || !IsValid(entry) ||
                entry!.ConversationId != conversationId || entry.MessageId != messageId)
            {
                Report("read", "invalid identity, metadata or version");
                return new(null, false);
            }
            return new(entry with { EndedAt = entry.EndedAt.ToUniversalTime() }, false);
        }
        catch (FileNotFoundException) { return new(null, true); }
        catch (DirectoryNotFoundException) { return new(null, true); }
        catch (Exception error) when (IsCacheFailure(error))
        {
            Report("read", error.GetType().Name);
            return new(null, false);
        }
    }

    private async Task<FileStream?> AcquireWriteLockAsync()
    {
        Directory.CreateDirectory(DirectoryPath);
        var elapsed = Stopwatch.StartNew();
        while (true)
        {
            try
            {
                return new FileStream(Path.Combine(DirectoryPath, ".write.lock"), FileMode.OpenOrCreate,
                    FileAccess.ReadWrite, FileShare.None, 1, FileOptions.Asynchronous);
            }
            catch (IOException) when (elapsed.Elapsed < LockTimeout)
            {
                // Lock contention yields the thread and expires; it must never stall the conversation indefinitely.
                // 写锁竞争通过异步等待让出线程，并在超时后结束，不能无限阻塞聊天。
                await Task.Delay(TimeSpan.FromMilliseconds(40)).ConfigureAwait(false);
            }
            catch (Exception error) when (IsCacheFailure(error))
            {
                Report("lock", error.GetType().Name);
                return null;
            }
        }
    }

    private string FilePath(Guid conversationId, Guid messageId) =>
        Path.Combine(DirectoryPath, conversationId.ToString("N"), messageId.ToString("N") + ".json");

    private static bool IsValid(MessageEndTimeEntry? entry) => entry is not null &&
        entry.ConversationId != Guid.Empty && entry.MessageId != Guid.Empty &&
        entry.Status is "completed" or "error" or "interrupted" &&
        entry.ContentHash is { Length: 64 } hash && hash.All(character => character is >= '0' and <= '9' or >= 'a' and <= 'f') &&
        entry.EndedAt > DateTimeOffset.UnixEpoch;

    private static bool IsCacheFailure(Exception error) => error is IOException or UnauthorizedAccessException or
        JsonException or NotSupportedException or ArgumentException or System.Security.SecurityException;

    private static void Report(string operation, string reason) =>
        Debug.WriteLine($"Message end-time cache {operation} skipped: {reason}.");
}
