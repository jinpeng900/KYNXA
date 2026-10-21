using System.Net;
using System.Text;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;

int checks = 0;
void Check(bool condition, string description)
{ if (!condition) throw new InvalidOperationException(description); checks++; }
async Task ExpectAsync<T>(Func<Task> action, string description) where T : Exception
{
    try { await action(); }
    catch (T) { checks++; return; }
    throw new InvalidOperationException(description);
}
var options = new JsonSerializerOptions(JsonSerializerDefaults.Web);
Guid conversation = Guid.NewGuid();
var snapshot = new ConversationUsageSnapshot("conversation", conversation, Guid.NewGuid(),
    4, 100, 20, 2, 2, 1, 50, 80, 1, 0.625, 123);
var cacheDetails = JsonSerializer.SerializeToNode(snapshot, options)!.AsObject();
cacheDetails["cacheCreationTokens"] = 10;
cacheDetails["uncachedInputTokens"] = 20;
cacheDetails["firstVisibleTextMs"] = 125;
cacheDetails["firstVisibleTextMsTotal"] = 250;
cacheDetails["knownFirstVisibleTextRequests"] = 2;
cacheDetails["knownCacheCreationRequests"] = 1;
cacheDetails["knownUncachedRequests"] = 1;
string body = cacheDetails.ToJsonString();
var handler = new FakeHandler((_, _) => Task.FromResult(Json(body)));
using var http = new HttpClient(handler) { BaseAddress = new Uri("http://usage.test.invalid") };
int readinessCalls = 0;
using var client = new UsageApiClient(http, _ => { readinessCalls++; return Task.CompletedTask; });
var actual = await client.ReadConversationAsync(conversation);
Check(actual == snapshot, "Known counts, cache denominator and pending/unknown states changed in transit.");
Check(handler.LastPath == $"/api/usage/summary?conversationId={conversation:D}", "Usage escaped the selected conversation scope.");
Check(readinessCalls == 1, "A background read must use the existing shared readiness entry.");
await ExpectAsync<ArgumentException>(() => client.ReadConversationAsync(Guid.Empty), "An unsaved chat must never issue a usage request.");
Check(handler.Calls == 1 && readinessCalls == 1, "An invalid chat touched transport or startup.");

async Task ReadVariantAsync(ConversationUsageSnapshot value)
{
    using var variantHttp = new HttpClient(new FakeHandler((_, _) => Task.FromResult(Json(JsonSerializer.Serialize(value, options)))))
        { BaseAddress = http.BaseAddress };
    using var variant = new UsageApiClient(variantHttp);
    await variant.ReadConversationAsync(conversation);
}
await ExpectAsync<InvalidDataException>(() => ReadVariantAsync(snapshot with { ConversationId = Guid.NewGuid() }), "Cross-chat usage was accepted.");
await ExpectAsync<InvalidDataException>(() => ReadVariantAsync(snapshot with { Scope = "project" }), "A project aggregate replaced a conversation projection.");
await ExpectAsync<InvalidDataException>(() => ReadVariantAsync(snapshot with { ProjectId = Guid.Empty }), "An invalid project identity was accepted.");
await ExpectAsync<InvalidDataException>(() => ReadVariantAsync(snapshot with { CacheHitRate = 1.1 }), "An impossible cache rate was accepted.");
await ExpectAsync<InvalidDataException>(() => ReadVariantAsync(snapshot with { CacheReadTokens = 81 }), "Cache hits exceeded their matching input.");
await ExpectAsync<InvalidDataException>(() => ReadVariantAsync(snapshot with { KnownTokenRequests = 5 }), "Complete usage exceeded dispatched requests.");
await ExpectAsync<InvalidDataException>(() => ReadVariantAsync(snapshot with { KnownCacheRequests = 5 }), "Cache coverage exceeded dispatched requests.");
await ExpectAsync<InvalidDataException>(() => ReadVariantAsync(snapshot with { CacheInputTokens = 0, CacheReadTokens = 0 }), "A non-null cache rate used a zero denominator.");
await ReadVariantAsync(snapshot with { ModelRequests = 0, KnownTokenRequests = 0, KnownCacheRequests = 0,
    InputTokens = 0, OutputTokens = 0, CacheReadTokens = 0, CacheInputTokens = 0, CacheHitRate = null,
    UnknownTokenRequests = 2, PendingRequests = 2 });
checks++;

// Missing fields cannot silently deserialize as exact zero; the UI reports unavailable instead.
// 缺字段不能静默解成精确零；界面应显示暂不可用，不伪造统计，也不中断聊天。
using (var missingHttp = new HttpClient(new FakeHandler((_, _) => Task.FromResult(Json(body.Replace("\"inputTokens\":100,", "")))))
    { BaseAddress = http.BaseAddress })
using (var missingClient = new UsageApiClient(missingHttp))
    await ExpectAsync<JsonException>(() => missingClient.ReadConversationAsync(conversation), "A missing count became a fabricated zero.");

using (var failureHttp = new HttpClient(new FakeHandler((_, _) => Task.FromResult(new HttpResponseMessage(HttpStatusCode.BadGateway)
    { Content = new StringContent("<html>untrusted upstream body</html>") }))) { BaseAddress = http.BaseAddress })
using (var failureClient = new UsageApiClient(failureHttp))
    await ExpectAsync<GatewayApiException>(() => failureClient.ReadConversationAsync(conversation), "A failed projection was accepted as usage.");

var entered = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
using (var cancelHttp = new HttpClient(new FakeHandler(async (_, token) =>
    { entered.TrySetResult(); await Task.Delay(Timeout.InfiniteTimeSpan, token); return Json(body); })) { BaseAddress = http.BaseAddress })
using (var cancelClient = new UsageApiClient(cancelHttp))
using (var cancellation = new CancellationTokenSource())
{
    var pending = cancelClient.ReadConversationAsync(conversation, cancellation.Token);
    await entered.Task;
    cancellation.Cancel();
    await ExpectAsync<OperationCanceledException>(() => pending, "Changing chats did not cancel the old background read.");
}
Console.WriteLine($"PASS: {checks} usage summary protocol/scope/unknown/cancellation checks; no gateway, model or user data accessed.");

static HttpResponseMessage Json(string value) => new(HttpStatusCode.OK)
    { Content = new StringContent(value, Encoding.UTF8, "application/json") };

sealed class FakeHandler(Func<HttpRequestMessage, CancellationToken, Task<HttpResponseMessage>> send) : HttpMessageHandler
{
    public int Calls { get; private set; }
    public string? LastPath { get; private set; }
    protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
    { Calls++; LastPath = request.RequestUri!.PathAndQuery; return send(request, cancellationToken); }
}
