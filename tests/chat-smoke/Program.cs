using System.Net;
using System.Net.Http.Json;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;

static void Check(bool condition, string message)
{
    if (!condition) throw new InvalidOperationException(message);
}

using var client = new MockChatClient();
await Task.WhenAll(new[] { "今天的天气怎么样？", "Tell me a story", "第一行\n第二行 😀" }.Select(async (question, index) =>
{
    Guid id = Guid.NewGuid();
    string mode = new[] { ChatPermissionModes.Ask, ChatPermissionModes.Smart, ChatPermissionModes.Full }[index];
    var reply = await client.ReplyAsync(new ChatRequest(id, question, "任意模型", mode), CancellationToken.None);
    Check(reply.ConversationId == id && reply.Role == "assistant" && reply.Content == "你好", "Reply contract or fixed answer is incorrect.");
}));
using var http = new HttpClient { BaseAddress = new Uri(Environment.GetEnvironmentVariable("KYNXA_MOCK_API_URL") ?? "http://127.0.0.1:5217") };
using var invalid = await http.PostAsJsonAsync("/api/chat", new ChatRequest(Guid.NewGuid(), "  "));
Check(invalid.StatusCode == HttpStatusCode.BadRequest, "Empty input should be rejected.");
using var invalidPermission = await http.PostAsJsonAsync("/api/chat", new ChatRequest(Guid.NewGuid(), "你好", PermissionMode: "unknown"));
Check(invalidPermission.StatusCode == HttpStatusCode.BadRequest, "Unknown permission mode should be rejected.");
using var legacyRequest = await http.PostAsJsonAsync("/api/chat", new { conversationId = Guid.NewGuid(), message = "兼容旧请求" });
Check(legacyRequest.IsSuccessStatusCode, "Requests without permissionMode should default to ask.");
using var cancellation = new CancellationTokenSource(30);
try
{
    await client.ReplyAsync(new ChatRequest(Guid.NewGuid(), "取消测试"), cancellation.Token);
    throw new InvalidOperationException("Cancellation was ignored.");
}
catch (OperationCanceledException) when (cancellation.IsCancellationRequested) { }

string dataRoot = Path.Combine(Path.GetTempPath(), "kynxa-chat-smoke-" + Guid.NewGuid().ToString("N"));
Directory.CreateDirectory(dataRoot);
var store = new ProjectStore(dataRoot);
Guid legacyId = Guid.NewGuid();
File.WriteAllText(Path.Combine(dataRoot, "chats.json"), JsonSerializer.Serialize(new[]
{
    new { Id = legacyId, Title = "旧聊天", Messages = new[] { "原有问题" } }
}));
var chats = store.LoadChats();
Check(chats[0].Messages[0].Role == "user" && chats[0].Messages[0].Content == "原有问题", "Legacy history was not preserved.");
chats[0].Messages.Add(new ChatMessageState { Role = "assistant", Content = "你好" });
store.SaveChats(chats);
var restored = store.LoadChats()[0];
Check(restored.Id == legacyId && restored.Messages.Select(m => m.Role).SequenceEqual(["user", "assistant"]), "Message roles changed after reload.");
Check(restored.Messages[1].Content == "你好", "Assistant reply was not persisted.");
var draftOnly = new ProjectChatState { Draft = "草稿保留测试-枫叶99\n尚未发送 😀" };
var empty = new ProjectChatState();
var whitespaceOnly = new ProjectChatState { Draft = " \t\r\n " };
var project = new ProjectState { Name = "存储测试", Chats = [draftOnly, empty, whitespaceOnly, restored] };
store.Save([project]);
store.SaveChats([draftOnly, empty, whitespaceOnly, restored]);
var reloadedWorkChats = store.Load()[0].Chats;
var reloadedStandaloneChats = store.LoadChats();
Check(reloadedWorkChats.Select(chat => chat.Id).SequenceEqual([draftOnly.Id, restored.Id]),
    "Work history must retain typed drafts and existing order while excluding blank new chats.");
Check(reloadedStandaloneChats.Select(chat => chat.Id).SequenceEqual([draftOnly.Id, restored.Id]),
    "Standalone history must retain typed drafts and existing order while excluding blank new chats.");
Check(reloadedWorkChats[0].Draft == draftOnly.Draft && reloadedStandaloneChats[0].Draft == draftOnly.Draft,
    "An unsent Unicode draft must survive save and reload without text loss.");
Check(reloadedWorkChats[0].CanPersist && reloadedStandaloneChats[0].CanPersist
    && reloadedWorkChats[0].Messages.Count == 0 && reloadedStandaloneChats[0].Messages.Count == 0,
    "Startup filtering must keep a restored typed draft without turning it into a submitted message.");
var folderless = new ProjectState
{
    Name = "不使用文件夹", IsFolderlessWorkspace = true,
    Chats = [new ProjectChatState { Title = "工作任务", Messages = [new ChatMessageState { Content = "开始工作" }] }]
};
store.Save([project, folderless]);
var loadedFolderless = store.Load().Single(item => item.IsFolderlessWorkspace);
Check(loadedFolderless.FolderPath is null && loadedFolderless.Chats.Count == 1, "Folderless work must remain independent of linked folders.");
Check(store.LoadChats().Select(chat => chat.Id).SequenceEqual([draftOnly.Id, legacyId]), "Work tasks must not enter ordinary chat history.");
Console.WriteLine("PASS: fixed HTTP reply, Unicode, concurrent conversation IDs, validation, cancellation, legacy migration, role persistence, typed-draft retention, and blank-chat exclusion.");
