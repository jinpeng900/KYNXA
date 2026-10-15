using System.Collections.Concurrent;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;

static void Check(bool condition, string message) { if (!condition) throw new Exception(message); }
var window = new ConversationHistoryWindow(maximumMessages: 4, maximumCharacters: 10000);
var pageChat = Guid.NewGuid();
ConversationHistoryPage Page(int first, string? older, string? newer) => new(pageChat,
    Enumerable.Range(first, 2).Select(index => new ChatMessageState { Content = $"PAGE_{index}" }).ToList(),
    6, older is not null, older, "same-snapshot", 0, false, newer, newer is not null, first, first + 2);
var latestPage = Page(4, "older-4", null);
window.Apply(latestPage, ConversationHistoryWindow.Measure(latestPage.Messages), false, true, []);
var middlePage = Page(2, "older-2", "newer-4");
window.Apply(middlePage, ConversationHistoryWindow.Measure(middlePage.Messages), false, false, []);
var oldestPage = Page(0, null, "newer-2");
var windowMessages = window.Apply(oldestPage, ConversationHistoryWindow.Measure(oldestPage.Messages), false, false, []);
Check(windowMessages.Select(message => message.Content).SequenceEqual(["PAGE_0", "PAGE_1", "PAGE_2", "PAGE_3"])
    && !window.HasOlder && window.NewerCursor == "newer-4", "Older navigation did not evict distant newer bodies or retain a newer return cursor.");
var pendingMessage = new ChatMessageState { Role = "assistant", Content = "ACTIVE_PENDING" };
windowMessages = window.Apply(latestPage, ConversationHistoryWindow.Measure(latestPage.Messages), true, false, [pendingMessage]);
Check(windowMessages.Select(message => message.Content).SequenceEqual(["PAGE_2", "PAGE_3", "PAGE_4", "PAGE_5", "ACTIVE_PENDING"])
    && window.OlderCursor == "older-2" && !window.HasNewer, "Newer navigation lost its pending message, older cursor, or bounded page window.");
Check(window.MessageCount == 4 && !window.ExceedsBudget, "Page data grew beyond the configured message window.");
window.Clear(); Check(window.Characters == 0 && window.MessageCount == 0, "History eviction retained page bodies.");
var byteWindow = new ConversationHistoryWindow(maximumMessages: 100, maximumCharacters: 12);
var widePage = Page(4, "older-wide", null);
widePage.Messages[0].Content = new string('文', 100);
var wideMessages = byteWindow.Apply(widePage, ConversationHistoryWindow.Measure(widePage.Messages), false, true, []);
Check(byteWindow.ExceedsBudget && wideMessages[0].Content.Length == 100,
    "An oversized durable message was silently truncated instead of remaining rereadable.");
byteWindow.Apply(middlePage, ConversationHistoryWindow.Measure(middlePage.Messages), false, false, []);
wideMessages = byteWindow.Apply(oldestPage, ConversationHistoryWindow.Measure(oldestPage.Messages), false, false, []);
Check(!wideMessages.Any(message => message.Content.Length == 100) && byteWindow.MessageCount == 4,
    "Character pressure failed to release the remote page while keeping both current scroll edges.");
var taskMessage = JsonSerializer.Deserialize<ChatMessageState>("{\"Role\":\"assistant\",\"Content\":\"已有正文\",\"Status\":\"completed\",\"ToolRun\":{\"taskCompletion\":{\"completionState\":\"verification-failed\"}}}")!;
Check(taskMessage.Status == "completed" && taskMessage.TaskCompletionState == "verification-failed", "History confused task verification failure with completed reply transport.");
var tcp = new TcpListener(IPAddress.Loopback, 0);
tcp.Start(); int port = ((IPEndPoint)tcp.LocalEndpoint).Port; tcp.Stop();
string? oldUrl = Environment.GetEnvironmentVariable("KYNXA_MODEL_API_URL");
Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", $"http://127.0.0.1:{port}");
using var listener = new HttpListener();
listener.Prefixes.Add($"http://127.0.0.1:{port}/"); listener.Start();
using var stopping = new CancellationTokenSource();
var uploads = new ConcurrentQueue<JsonElement>();
var historyRequests = new ConcurrentQueue<string>();
var enteredFirstPut = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
var releaseFirstPut = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
var enteredBlockedPut = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
var releaseBlockedPut = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
var handlers = new ConcurrentBag<Task>();
int revision = 10, oldProtocol = 0, forceConflict = 0, blockNextPut = 0;
var storedChat = new ProjectChatState { Title = "已有记录", Messages = [new() { Content = "已有问题" }, new() { Role = "assistant", Content = "正式回复", DurationMs = 5678 }] };
var canonical = new ConversationCatalog(revision, [], [storedChat]);
var server = Task.Run(async () =>
{
    while (!stopping.IsCancellationRequested)
    {
        try { var context = await listener.GetContextAsync().WaitAsync(stopping.Token); handlers.Add(Handle(context)); }
        catch (OperationCanceledException) { break; }
    }
});
async Task Handle(HttpListenerContext context)
{
    try
    {
        object result;
        if (context.Request.Url!.AbsolutePath == "/health")
            result = new { service = "kynxa-model-gateway", status = "ok", runtime = "rust", dataLayoutVersion = 1, memoryProtocol = 1, contextProtocol = 3, agentProtocol = 5, officialToolsProtocol = 2, hostTerminalProtocol = 3, browserAutomationProtocol = 2, extensionStorageProtocol = 1, toolStreamProtocol = 3, retrievalProtocol = 1, conversationProtocol = Volatile.Read(ref oldProtocol) == 0 ? 1 : 0 };
        else if (context.Request.Url.AbsolutePath.EndsWith("/messages", StringComparison.Ordinal))
        {
            historyRequests.Enqueue(context.Request.Url.Query);
            var chatId = Guid.Parse(context.Request.Url.AbsolutePath.Split('/')[3]);
            var history = canonical.Chats.Single(chat => chat.Id == chatId).Messages;
            result = new ConversationHistoryPage(chatId, history, history.Count, false, null, "synthetic-history", 0, false);
        }
        else if (context.Request.HttpMethod == "PUT")
        {
            using var document = await JsonDocument.ParseAsync(context.Request.InputStream);
            var payload = document.RootElement.Clone(); uploads.Enqueue(payload);
            if (uploads.Count == 1) { enteredFirstPut.SetResult(); await releaseFirstPut.Task; }
            if (Interlocked.Exchange(ref blockNextPut, 0) == 1) { enteredBlockedPut.SetResult(); await releaseBlockedPut.Task; }
            if (payload.GetProperty("Revision").GetInt32() != revision || Volatile.Read(ref forceConflict) == 1)
            {
                context.Response.StatusCode = 409; result = new { error = "conflict" };
            }
            else
            {
                if (payload.TryGetProperty("Chats", out var list))
                {
                    var received = JsonSerializer.Deserialize<List<ProjectChatState>>(list)!;
                    foreach (var chat in received)
                    {
                        var prior = canonical.Chats.FirstOrDefault(item => item.Id == chat.Id);
                        if (prior is not null) chat.Messages = prior.Messages.Concat(chat.Messages.Where(message => prior.Messages.All(old => old.Id != message.Id))).ToList();
                    }
                    canonical = canonical with { Chats = received };
                }
                if (payload.TryGetProperty("Projects", out var projects)) canonical = canonical with { Projects = JsonSerializer.Deserialize<List<ProjectState>>(projects)! };
                canonical = canonical with { Revision = ++revision }; result = canonical;
            }
        }
        else result = canonical;
        if (result is ConversationCatalog catalog && context.Request.QueryString["includeMessages"] == "false")
            result = catalog with { Chats = catalog.Chats.Select(chat => new ProjectChatState
                { Id = chat.Id, Title = chat.Title, Draft = chat.Draft, IsSample = chat.IsSample, Messages = [] }).ToList() };
        context.Response.ContentType = "application/json";
        await context.Response.OutputStream.WriteAsync(Encoding.UTF8.GetBytes(JsonSerializer.Serialize(result)));
    }
    finally { context.Response.Close(); }
}
try
{
    string dataPath = Path.Combine(Path.GetTempPath(), "kynxa-client-storage-" + Guid.NewGuid().ToString("N"));
    using var store = new ProjectStore(dataPath);
    var loaded = await store.LoadAsync();
    Check(loaded.Chats[0].Messages[1].Content == "正式回复", "Did not read authoritative transcript.");
    Check(loaded.Chats[0].Messages[1].DurationMs == 5678, "Did not read authoritative generation duration.");
    var chat = loaded.Chats[0];
    var user = new ChatMessageState { Content = "新问题" }; chat.Messages.Add(user);
    chat.Messages.Add(new() { Role = "assistant", Content = "UI must not persist this", Status = "streaming" });
    var save = store.SaveChatsAsync([chat, new() { Draft = "未发送" }]);
    await enteredFirstPut.Task.WaitAsync(TimeSpan.FromSeconds(5));
    user.Content = "later UI mutation";
    var project = new ProjectState { Name = "项目", Chats = [new() { Draft = "empty" }] };
    var nextSave = store.SaveAsync([project]);
    releaseFirstPut.SetResult();
    await Task.WhenAll(save, nextSave);
    var recorded = uploads.ToArray();
    Check(recorded.Length == 2 && recorded[0].GetProperty("Revision").GetInt32() == 10 && recorded[1].GetProperty("Revision").GetInt32() == 11, "Concurrent metadata writes did not use consecutive revisions.");
    var sentChats = recorded[0].GetProperty("Chats");
    Check(sentChats.GetArrayLength() == 1, "Draft-only chat persisted.");
    var messages = sentChats[0].GetProperty("Messages");
    Check(messages.GetArrayLength() == 1 && messages[0].GetProperty("Id").GetGuid() == user.Id && messages[0].GetProperty("Content").GetString() == "新问题", "Assistant or known user was uploaded, or snapshot mutated in flight.");
    Check(!recorded[0].TryGetProperty("Projects", out _) && !recorded[1].TryGetProperty("Chats", out _), "Unrelated metadata scope would be replaced.");
    Check(recorded[1].GetProperty("Projects")[0].GetProperty("Chats").GetArrayLength() == 0, "Empty project chat persisted.");
    chat.Title = "重命名";
    await store.SaveChatsAsync([chat]);
    Check(uploads.Last().GetProperty("Chats")[0].GetProperty("Messages").GetArrayLength() == 0, "Rename resubmitted history.");
    Check(!Directory.Exists(dataPath), "Desktop wrote an independent chat store.");
    var reloaded = await store.LoadAsync();
    Check(reloaded.Chats[0].Messages[1].Content == "正式回复" && reloaded.Chats[0].Messages.All(message => message.Content != "UI must not persist this"), "UI overwrote authoritative reply.");
    Check(reloaded.Chats[0].Messages[1].DurationMs == 5678, "Reopening changed generation duration.");

    int beforeClose = uploads.Count;
    await store.SaveCatalogAsync(reloaded.Projects, reloaded.Chats);
    Check(uploads.Count == beforeClose, "An unchanged exit snapshot performed a network write.");
    reloaded.Projects[0].Name = "退出项目";
    reloaded.Chats[0].Draft = "退出草稿";
    await store.SaveCatalogAsync(reloaded.Projects, reloaded.Chats);
    Check(uploads.Count == beforeClose + 1 && uploads.Last().TryGetProperty("Projects", out _) && uploads.Last().TryGetProperty("Chats", out _),
        "Exit did not commit both sidebar scopes in one revision.");
    Check(uploads.Last().GetProperty("Chats")[0].GetProperty("Draft").GetString() == "退出草稿", "Exit discarded a persisted chat draft.");
    await store.SaveCatalogAsync(reloaded.Projects, reloaded.Chats);
    Check(uploads.Count == beforeClose + 1, "A saved exit snapshot was needlessly submitted twice.");
    reloaded.Projects[0].FolderPath = Path.Combine(dataPath, "folder-change");
    await store.SaveCatalogAsync(reloaded.Projects, reloaded.Chats);
    Check(uploads.Count == beforeClose + 2, "The unchanged-save shortcut lost a work-folder change.");

    // A deadline may cancel a queued exit without releasing another write's gate or replaying it.
    // 退出期限可以取消排队请求，但不能释放别的写入占有的锁，也不能重放该写入。
    blockNextPut = 1;
    reloaded.Chats[0].Title = "写入中";
    var blockedSave = store.SaveChatsAsync(reloaded.Chats);
    await enteredBlockedPut.Task.WaitAsync(TimeSpan.FromSeconds(5));
    int queuedUploads = uploads.Count;
    using (var deadline = new CancellationTokenSource(TimeSpan.FromMilliseconds(100)))
    {
        try { await store.SaveCatalogAsync(reloaded.Projects, reloaded.Chats, deadline.Token); throw new Exception("Queued exit ignored cancellation."); }
        catch (OperationCanceledException) { Check(uploads.Count == queuedUploads, "Canceled queued exit dispatched a write."); }
    }
    releaseBlockedPut.SetResult();
    await blockedSave;
    await store.SaveCatalogAsync(reloaded.Projects, reloaded.Chats);
    Check(uploads.Count == queuedUploads, "Canceled exit corrupted the write gate or forced a duplicate save.");
    using var pagedStore = new ProjectStore(dataPath);
    var metadata = await pagedStore.LoadMetadataAsync();
    Check(metadata.Chats[0].Messages.Count == 0 && metadata.Chats[0].CanPersist,
        "Metadata-only persisted chats were mistaken for disposable drafts.");
    var historyPage = await pagedStore.LoadHistoryPageAsync(metadata.Chats[0].Id, limit: 40, maximumBytes: 1024 * 1024);
    Check(historyRequests.Last().Contains("limit=40") && historyRequests.Last().Contains("maxBytes=1048576")
        && !historyRequests.Last().Contains("cursor="), "Initial history did not request a bounded latest page without an old cursor.");
    Check(historyPage.Messages.Any(message => message.Content == "正式回复"), "Native history page did not deserialize its messages.");
    metadata.Chats[0].Messages = historyPage.Messages;
    Check(pagedStore.CanReleaseHistory(metadata.Chats[0]), "Durable paged messages could not release their display projection.");
    pagedStore.ReleaseHistoryProjection(metadata.Chats[0].Id);
    pagedStore.AcceptHistoryPage(historyPage);
    Check(pagedStore.CanReleaseHistory(metadata.Chats[0]), "Restored latest-page cache lost its durable user-message identities.");
    var unsaved = new ChatMessageState { Content = "尚未提交的用户消息" };
    metadata.Chats[0].Messages.Add(unsaved);
    Check(!pagedStore.CanReleaseHistory(metadata.Chats[0]) && pagedStore.UnpersistedHistoryTail(metadata.Chats[0]).Contains(unsaved),
        "Projection eviction admitted an unsaved user message.");
    metadata.Chats[0].Messages.Remove(unsaved);
    metadata.Chats[0].Draft = "分页后的草稿";
    await pagedStore.SaveChatsAsync(metadata.Chats);
    Check(uploads.Last().GetProperty("Chats")[0].GetProperty("Messages").GetArrayLength() == 0,
        "Loading a history page caused already saved user messages to be uploaded again.");
    forceConflict = 1;
    try { await store.SaveChatsAsync([chat]); throw new Exception("Conflict ignored."); }
    catch (InvalidOperationException error) { Check(error.Message.Contains("其他窗口"), "Conflict was not actionable."); }
    oldProtocol = 1;
    try { await store.LoadAsync(); throw new Exception("Old gateway accepted."); }
    catch (InvalidOperationException error) { Check(error.Message.Contains("旧网关"), "Old gateway did not explain incompatibility."); }
    Console.WriteLine("PASS: bounded bidirectional page windows, oversized-source preservation, durable/unsaved eviction guards, historical task outcomes, canonical loads, immutable snapshots, serialized revisions, scope isolation, draft exclusion, assistant authority, only-new-user uploads, combined/no-op exit saves, folder changes, queued cancellation, conflict and protocol rejection; no desktop transcript files.");
}
finally
{
    releaseFirstPut.TrySetResult(); releaseBlockedPut.TrySetResult(); stopping.Cancel(); await server; await Task.WhenAll(handlers);
    Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", oldUrl); listener.Stop();
}
