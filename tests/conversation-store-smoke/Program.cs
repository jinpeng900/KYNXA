using System.Collections.Concurrent;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;

static void Check(bool condition, string message) { if (!condition) throw new Exception(message); }
var tcp = new TcpListener(IPAddress.Loopback, 0);
tcp.Start(); int port = ((IPEndPoint)tcp.LocalEndpoint).Port; tcp.Stop();
string? oldUrl = Environment.GetEnvironmentVariable("KYNXA_MODEL_API_URL");
Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", $"http://127.0.0.1:{port}");
using var listener = new HttpListener();
listener.Prefixes.Add($"http://127.0.0.1:{port}/"); listener.Start();
using var stopping = new CancellationTokenSource();
var uploads = new ConcurrentQueue<JsonElement>();
var enteredFirstPut = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
var releaseFirstPut = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
var handlers = new ConcurrentBag<Task>();
int revision = 10, oldProtocol = 0, forceConflict = 0;
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
            result = new { service = "kynxa-model-gateway", status = "ok", dataLayoutVersion = 1, memoryProtocol = 1, contextProtocol = 3, agentProtocol = 5, officialToolsProtocol = 2, hostTerminalProtocol = 3, browserAutomationProtocol = 2, extensionStorageProtocol = 1, toolStreamProtocol = 3, conversationProtocol = Volatile.Read(ref oldProtocol) == 0 ? 1 : 0 };
        else if (context.Request.HttpMethod == "PUT")
        {
            using var document = await JsonDocument.ParseAsync(context.Request.InputStream);
            var payload = document.RootElement.Clone(); uploads.Enqueue(payload);
            if (uploads.Count == 1) { enteredFirstPut.SetResult(); await releaseFirstPut.Task; }
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
    forceConflict = 1;
    try { await store.SaveChatsAsync([chat]); throw new Exception("Conflict ignored."); }
    catch (InvalidOperationException error) { Check(error.Message.Contains("其他窗口"), "Conflict was not actionable."); }
    oldProtocol = 1;
    try { await store.LoadAsync(); throw new Exception("Old gateway accepted."); }
    catch (InvalidOperationException error) { Check(error.Message.Contains("旧网关"), "Old gateway did not explain incompatibility."); }
    Console.WriteLine("PASS: canonical loads, immutable snapshots, serialized revisions, scope isolation, draft exclusion, assistant authority, only-new-user uploads, conflict and protocol rejection; no desktop transcript files.");
}
finally
{
    releaseFirstPut.TrySetResult(); stopping.Cancel(); await server; await Task.WhenAll(handlers);
    Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", oldUrl); listener.Stop();
}
