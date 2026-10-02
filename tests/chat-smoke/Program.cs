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

Guid legacyId = Guid.NewGuid();
var chats = JsonSerializer.Deserialize<List<ProjectChatState>>(JsonSerializer.Serialize(new[]
{
    new { Id = legacyId, Title = "旧聊天", Messages = new[] { "原有问题" } }
}))!;
Check(chats[0].Messages[0].Role == "user" && chats[0].Messages[0].Content == "原有问题", "Legacy history was not preserved.");
chats[0].Messages.Add(new ChatMessageState { Role = "assistant", Content = "你好" });
var restored = JsonSerializer.Deserialize<List<ProjectChatState>>(JsonSerializer.Serialize(chats))![0];
Check(restored.Id == legacyId && restored.Messages.Select(m => m.Role).SequenceEqual(["user", "assistant"]), "Message roles changed after reload.");
Check(restored.Messages[1].Content == "你好", "Assistant reply was not persisted.");
var empty = new ProjectChatState { Draft = "未发送草稿" };
Check(!empty.CanPersist && restored.CanPersist, "Empty chats should not be persisted.");
Console.WriteLine("PASS: fixed HTTP reply, Unicode, concurrent conversation IDs, validation, cancellation, legacy conversion, role serialization, and empty-chat exclusion. Gateway persistence is covered by conversation-store-smoke.");
