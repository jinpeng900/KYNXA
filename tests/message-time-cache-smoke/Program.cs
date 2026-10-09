using System.Diagnostics;
using System.Globalization;
using System.Reflection;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using KYNXA_Desktop.Services;

if (args.Length > 0) return await RunChildAsync(args);

string temporaryRoot = Path.Combine(Path.GetTempPath(), "kynxa-message-time-cache-smoke-" + Guid.NewGuid().ToString("N"));
Directory.CreateDirectory(temporaryRoot);
int checks = 0;
var firstTime = new DateTimeOffset(2026, 10, 8, 12, 30, 0, TimeSpan.Zero);
string fingerprint = Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes("Synthetic reply."))).ToLowerInvariant();
Guid chat = Guid.NewGuid(), message = Guid.NewGuid();
var first = new MessageEndTimeEntry(chat, message, "completed", fingerprint, firstTime);

try
{
    string desktop = Path.Combine(temporaryRoot, "Desktop");
    var cache = new MessageEndTimeCache(desktop);
    Check(await cache.ReadAsync(chat, message) is null && !Directory.Exists(cache.DirectoryPath),
        "reading a missing entry neither fails nor creates directories");
    Check(await cache.WriteAsync(first), "the first terminal observation is persisted");
    Check(await new MessageEndTimeCache(desktop).ReadAsync(chat, message) == first,
        "a new cache instance reads the stored observation");

    using (var stored = JsonDocument.Parse(await File.ReadAllBytesAsync(EntryPath(cache, chat, message))))
    {
        var names = stored.RootElement.EnumerateObject().Select(property => property.Name).Order().ToArray();
        Check(names.SequenceEqual(new[] { "contentHash", "conversationId", "endedAt", "messageId", "schemaVersion", "status" }.Order()),
            "the persisted file contains only the version, identities, status, fingerprint and observation time");
    }

    string restartDesktop = Path.Combine(temporaryRoot, "Restart", "Desktop");
    Check(await ChildAsync("write", restartDesktop, chat.ToString(), message.ToString(), firstTime.ToString("O"), fingerprint) == "true",
        "an independent process persists the observation");
    string restored = await ChildAsync("read", restartDesktop, chat.ToString(), message.ToString());
    Check(JsonSerializer.Deserialize<MessageEndTimeEntry>(restored) == first,
        "a subsequent independent process restores the exact timestamp after the writer exits");

    Guid otherChat = Guid.NewGuid();
    var otherConversation = first with { ConversationId = otherChat, EndedAt = firstTime.AddMinutes(1) };
    Check(await cache.WriteAsync(otherConversation) && await cache.ReadAsync(chat, message) == first &&
        await cache.ReadAsync(otherChat, message) == otherConversation, "identical message IDs remain isolated between conversations");
    var otherRoot = new MessageEndTimeCache(Path.Combine(temporaryRoot, "Other", "Desktop"));
    Check(await otherRoot.ReadAsync(chat, message) is null && await otherRoot.WriteAsync(first with { EndedAt = firstTime.AddMinutes(2) }) &&
        await cache.ReadAsync(chat, message) == first, "different desktop roots neither read nor overwrite each other's observations");

    var later = first with { EndedAt = firstTime.AddHours(1) };
    Check(await cache.WriteAsync(later) && await cache.ReadAsync(chat, message) == first,
        "later duplicate observations preserve the first recorded instant");
    var parallelEntries = Enumerable.Range(0, 24).Select(index => first with
    {
        MessageId = Guid.NewGuid(), EndedAt = firstTime.AddSeconds(index)
    }).ToArray();
    var concurrentWrites = parallelEntries.Select(entry => new MessageEndTimeCache(desktop).WriteAsync(entry))
        .Concat(Enumerable.Range(0, 12).Select(_ => new MessageEndTimeCache(desktop).WriteAsync(later)));
    Check((await Task.WhenAll(concurrentWrites)).All(result => result), "concurrent cache instances complete all independent writes");
    Check((await Task.WhenAll(parallelEntries.Select(entry => cache.ReadAsync(chat, entry.MessageId))))
        .SequenceEqual(parallelEntries) && await cache.ReadAsync(chat, message) == first,
        "concurrent writes retain every entry and the first timestamp for shared fingerprints");

    Guid[] childMessages = Enumerable.Range(0, 4).Select(_ => Guid.NewGuid()).ToArray();
    var childWrites = await Task.WhenAll(childMessages.Select(identifier => ChildAsync("write", desktop,
        chat.ToString(), identifier.ToString(), firstTime.ToString("O"), fingerprint)));
    Check(childWrites.All(result => result == "true") &&
        (await Task.WhenAll(childMessages.Select(identifier => cache.ReadAsync(chat, identifier)))).All(entry => entry is not null),
        "independent concurrent processes share the root lock without losing entries");

    var forcedRetry = first with { EndedAt = firstTime.AddHours(2) };
    Check(await cache.WriteAsync(forcedRetry, replaceExisting: true) && await cache.ReadAsync(chat, message) == forcedRetry,
        "an explicit retry replaces an old observation even with identical IDs, status and content");
    Check(await cache.RemoveAsync(chat, message) && await cache.ReadAsync(chat, message) is null &&
        await cache.WriteAsync(later) && await cache.ReadAsync(chat, message) == later,
        "removing one message permits a fresh same-content attempt to record its own time");
    Check(await cache.ReadAsync(otherChat, message) == otherConversation,
        "message removal does not affect the same ID in another conversation");
    Check(await cache.RemoveAsync(chat, message) && await cache.RemoveAsync(chat, message),
        "removing an absent entry is idempotent");

    var changed = first with { Status = "interrupted", ContentHash = new string('b', 64), EndedAt = firstTime.AddHours(3) };
    Check(await cache.WriteAsync(first) && await cache.WriteAsync(changed) && await cache.ReadAsync(chat, message) == changed,
        "a changed terminal status or fingerprint replaces an obsolete valid observation");
    var offsetEntry = first with { MessageId = Guid.NewGuid(), EndedAt = firstTime.ToOffset(TimeSpan.FromHours(8)) };
    Check(await cache.WriteAsync(offsetEntry) && (await cache.ReadAsync(chat, offsetEntry.MessageId))?.EndedAt.Offset == TimeSpan.Zero,
        "stored observation timestamps use UTC");

    foreach (var invalid in new[]
    {
        first with { ConversationId = Guid.Empty }, first with { MessageId = Guid.Empty },
        first with { Status = "streaming" }, first with { ContentHash = "not-a-hash" },
        first with { ContentHash = new string('A', 64) }, first with { EndedAt = DateTimeOffset.UnixEpoch }
    })
        Check(!await cache.WriteAsync(invalid), "invalid identities, status, fingerprint or timestamps are rejected");
    Check(await cache.ReadAsync(Guid.Empty, message) is null && !await cache.RemoveAsync(Guid.Empty),
        "invalid read and removal identities are rejected without file operations");

    Guid damagedMessage = Guid.NewGuid(), futureMessage = Guid.NewGuid(), largeMessage = Guid.NewGuid(), mismatchMessage = Guid.NewGuid();
    byte[] corrupt = Encoding.UTF8.GetBytes("{invalid JSON\r\n");
    byte[] future = JsonSerializer.SerializeToUtf8Bytes(new
    {
        schemaVersion = 99, conversationId = chat, messageId = futureMessage,
        status = "completed", contentHash = fingerprint, endedAt = firstTime
    });
    byte[] oversized = Encoding.UTF8.GetBytes(new string('x', 17 * 1024));
    byte[] mismatched = JsonSerializer.SerializeToUtf8Bytes(new
    {
        schemaVersion = 1, conversationId = otherChat, messageId = mismatchMessage,
        status = "completed", contentHash = fingerprint, endedAt = firstTime
    });
    foreach (var protectedFile in new[]
    {
        (Id: damagedMessage, Bytes: corrupt), (Id: futureMessage, Bytes: future),
        (Id: largeMessage, Bytes: oversized), (Id: mismatchMessage, Bytes: mismatched)
    })
    {
        string path = EntryPath(cache, chat, protectedFile.Id);
        await File.WriteAllBytesAsync(path, protectedFile.Bytes);
        Check(await cache.ReadAsync(chat, protectedFile.Id) is null &&
            !await cache.WriteAsync(first with { MessageId = protectedFile.Id }) &&
            !await cache.WriteAsync(first with { MessageId = protectedFile.Id }, replaceExisting: true) &&
            !await cache.RemoveAsync(chat, protectedFile.Id) &&
            (await File.ReadAllBytesAsync(path)).SequenceEqual(protectedFile.Bytes),
            "corrupt, future, oversized and misplaced records remain byte-for-byte intact during read, write, retry and removal");
    }

    string unrelatedPath = Path.Combine(cache.DirectoryPath, chat.ToString("N"), "notes.json");
    string nestedDirectory = Path.Combine(cache.DirectoryPath, chat.ToString("N"), "unrelated");
    Directory.CreateDirectory(nestedDirectory);
    await File.WriteAllTextAsync(unrelatedPath, "Synthetic unrelated file.");
    string nestedPath = Path.Combine(nestedDirectory, message.ToString("N") + ".json");
    await File.WriteAllTextAsync(nestedPath, "Synthetic nested file.");
    Check(!await cache.RemoveAsync(chat) && await cache.ReadAsync(chat, message) is null &&
        File.Exists(unrelatedPath) && File.Exists(nestedPath) &&
        (await File.ReadAllBytesAsync(EntryPath(cache, chat, futureMessage))).SequenceEqual(future) &&
        await cache.ReadAsync(otherChat, message) == otherConversation,
        "conversation removal deletes only valid direct message files and preserves protected, unrelated and other-conversation data");

    Check(await cache.WriteAsync(first), "a valid observation can be created beside protected files");
    var timeout = Stopwatch.StartNew();
    using (var blockedWriteLock = new FileStream(Path.Combine(cache.DirectoryPath, ".write.lock"), FileMode.Open,
        FileAccess.Read, FileShare.Read))
    {
        Check(await cache.ReadAsync(chat, message) == first,
            "existing observations remain readable when writing the cache is unavailable");
        Check(!await cache.WriteAsync(later, replaceExisting: true) && timeout.Elapsed < TimeSpan.FromSeconds(6),
            "write contention returns failure within its bounded retry window");
        Check(await cache.ReadAsync(chat, message) == first,
            "a failed write neither damages nor hides a previously readable observation");
    }
    Check(await cache.WriteAsync(later, replaceExisting: true), "writing recovers after the external lock is released");
    using (var lockedMessage = new FileStream(EntryPath(cache, chat, message), FileMode.Open,
        FileAccess.Read, FileShare.Read))
    {
        Check(!await cache.WriteAsync(forcedRetry, replaceExisting: true) && await cache.ReadAsync(chat, message) == later,
            "an atomic replacement failure preserves the previous readable file");
        Check(!Directory.EnumerateFiles(cache.DirectoryPath, "*.tmp", SearchOption.AllDirectories).Any(),
            "a failed atomic replacement cleans up its unique temporary file");
    }
    Check(!Directory.EnumerateFiles(cache.DirectoryPath, "*.tmp", SearchOption.AllDirectories).Any(),
        "successful and failed writes leave no temporary files");

    string blockedDesktop = Path.Combine(temporaryRoot, "BlockedDesktop");
    await File.WriteAllTextAsync(blockedDesktop, "Synthetic file blocking directory creation.");
    var unavailable = new MessageEndTimeCache(blockedDesktop);
    Check(await unavailable.ReadAsync(chat, message) is null && !await unavailable.WriteAsync(first),
        "an unusable storage directory returns a cache miss or write failure without throwing");

    await FacadeChecks.RunAsync(Path.Combine(temporaryRoot, "Facade"), Check);
    Console.WriteLine($"PASS: {checks} message end-time cache and facade checks.");
    return 0;
}
finally
{
    // Cleanup is confined to this generated fixture root under the system temporary directory.
    // 清理仅针对本夹具在系统临时目录中生成的独立根目录。
    string resolved = Path.GetFullPath(temporaryRoot);
    string temporaryParent = Path.GetFullPath(Path.GetTempPath()).TrimEnd(Path.DirectorySeparatorChar);
    if (Path.GetDirectoryName(resolved)?.TrimEnd(Path.DirectorySeparatorChar) == temporaryParent &&
        Path.GetFileName(resolved).StartsWith("kynxa-message-time-cache-smoke-", StringComparison.Ordinal))
        Directory.Delete(resolved, recursive: true);
}

void Check(bool condition, string description)
{
    if (!condition) throw new InvalidOperationException("FAIL: " + description);
    checks++;
    Console.WriteLine("PASS: " + description);
}

static string EntryPath(MessageEndTimeCache cache, Guid conversationId, Guid messageId) =>
    Path.Combine(cache.DirectoryPath, conversationId.ToString("N"), messageId.ToString("N") + ".json");

static async Task<string> ChildAsync(params string[] arguments)
{
    var start = new ProcessStartInfo("dotnet")
    {
        UseShellExecute = false, CreateNoWindow = true, RedirectStandardOutput = true, RedirectStandardError = true
    };
    start.ArgumentList.Add(Assembly.GetExecutingAssembly().Location);
    foreach (string argument in arguments) start.ArgumentList.Add(argument);
    using var process = Process.Start(start) ?? throw new InvalidOperationException("The cache fixture child did not start.");
    Task<string> output = process.StandardOutput.ReadToEndAsync(), error = process.StandardError.ReadToEndAsync();
    using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(20));
    await process.WaitForExitAsync(timeout.Token);
    string result = await output;
    if (process.ExitCode != 0) throw new InvalidOperationException("The cache fixture child failed: " + await error);
    return result.Trim();
}

static async Task<int> RunChildAsync(string[] arguments)
{
    var cache = new MessageEndTimeCache(arguments[1]);
    Guid conversationId = Guid.Parse(arguments[2]), messageId = Guid.Parse(arguments[3]);
    if (arguments[0] == "write")
    {
        var entry = new MessageEndTimeEntry(conversationId, messageId, "completed", arguments[5],
            DateTimeOffset.Parse(arguments[4], CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind));
        Console.WriteLine(await cache.WriteAsync(entry) ? "true" : "false");
        return 0;
    }
    if (arguments[0] == "read")
    {
        Console.WriteLine(JsonSerializer.Serialize(await cache.ReadAsync(conversationId, messageId)));
        return 0;
    }
    throw new ArgumentException("Unsupported fixture child command.");
}
