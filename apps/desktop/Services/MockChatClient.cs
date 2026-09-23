using System.Net.Http;
using System.Net.Http.Json;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

public sealed class MockChatClient : IDisposable
{
    private readonly HttpClient _http = new()
    {
        BaseAddress = new Uri(Environment.GetEnvironmentVariable("KYNXA_MOCK_API_URL") ?? "http://127.0.0.1:5217"),
        Timeout = TimeSpan.FromSeconds(15)
    };

    public async Task<ChatReply> ReplyAsync(ChatRequest request, CancellationToken cancellationToken)
    {
        using var response = await _http.PostAsJsonAsync("/api/chat", request, cancellationToken);
        response.EnsureSuccessStatusCode();
        var reply = await response.Content.ReadFromJsonAsync<ChatReply>(cancellationToken);
        if (reply is null || reply.ConversationId != request.ConversationId || reply.Role != "assistant" || string.IsNullOrEmpty(reply.Content))
            throw new InvalidDataException("模拟接口返回了无效的回复。");
        return reply;
    }

    public void Dispose() => _http.Dispose();
}
