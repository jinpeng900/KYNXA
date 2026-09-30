using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Encodings.Web;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;

static void Check(bool value, string name) { if (!value) throw new InvalidOperationException(name); }
static async Task ExpectAsync<T>(Func<Task> action, string name) where T : Exception
{
    try { await action(); }
    catch (T) { return; }
    throw new InvalidOperationException(name);
}
var json = new JsonSerializerOptions(JsonSerializerDefaults.Web) { Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping };
Guid conversationId = Guid.NewGuid(), requestId = Guid.NewGuid();
var createdAt = DateTimeOffset.UtcNow;
ChatStreamEvent Event(string type, string? delta = null, string? content = null, string? reasoning = null, string? error = null) =>
    new(type, conversationId, requestId, createdAt, delta, content, reasoning, error);
string Frame(ChatStreamEvent item) => "event: " + item.Type + "\r\ndata: " + JsonSerializer.Serialize(item, json) + "\r\n\r\n";
string prefix = ": heartbeat\r\n\r\n" + Frame(Event("started"));
async Task<List<ChatStreamEvent>> Read(string wire)
{
    using var stream = new FragmentedStream(Encoding.UTF8.GetBytes(wire));
    var received = new List<ChatStreamEvent>();
    await foreach (var item in ChatStreamReader.ReadAsync(stream, conversationId, requestId)) received.Add(item);
    return received;
}
var complete = Event("completed", content: "你好😀\n最终 **答案**", reasoning: "先核对 $1+1=2$。");
string pretty = JsonSerializer.Serialize(complete, new JsonSerializerOptions(json) { WriteIndented = true });
string multilineFrame = string.Join('\n', pretty.Split('\n').Select(line => "data: " + line)) + "\n\n";
var result = await Read(prefix + Frame(Event("reasoning_delta", "先核对 $1+1=2$。")) +
    Frame(Event("text_delta", "你好😀\n")) + Frame(Event("text_delta", "草稿")) + multilineFrame);
Check(result.Count == 5 && result[1].Delta!.Contains("$1+1=2$") && result[2].Delta == "你好😀\n", "Unicode fragmentation or multiline SSE failed");
Check(result[^1].Content == complete.Content, "Final canonical content was lost");
Check((await Read(prefix + Frame(Event("error", content: "已收到", reasoning: "思考", error: "连接中断"))))[^1].Error == "连接中断", "Error event lost partial content");
Check((await Read(prefix + Frame(Event("interrupted", content: "部分"))))[^1].Type == "interrupted", "Interrupted terminal not recognized");
await ExpectAsync<EndOfStreamException>(async () => { await Read(prefix + Frame(Event("text_delta", "部分"))); }, "Premature EOF accepted");
await ExpectAsync<InvalidDataException>(async () => { await Read(Frame(Event("text_delta", "无开始事件"))); }, "Missing start accepted");
await ExpectAsync<InvalidDataException>(async () => { await Read(prefix + Frame(Event("started"))); }, "Duplicate start accepted");
await ExpectAsync<InvalidDataException>(async () => { await Read(prefix + Frame(Event("text_delta", "其他会话") with { ConversationId = Guid.NewGuid() })); }, "Wrong conversation accepted");
await ExpectAsync<InvalidDataException>(async () => { await Read(prefix + Frame(Event("text_delta", "其他请求") with { RequestId = Guid.NewGuid() })); }, "Wrong request accepted");
await ExpectAsync<InvalidDataException>(async () => { await Read(prefix + Frame(Event("completed"))); }, "Missing final content accepted");
await ExpectAsync<InvalidDataException>(async () => { await Read(prefix + "data: {broken}\n\n"); }, "Malformed JSON accepted");
using (var cancellation = new CancellationTokenSource(30))
using (var stream = new WaitingStream())
{
    await ExpectAsync<OperationCanceledException>(async () =>
    {
        await foreach (var _ in ChatStreamReader.ReadAsync(stream, conversationId, requestId, cancellation.Token)) { }
    }, "Cancellation did not interrupt a pending read");
}

var legacyText = JsonSerializer.Deserialize<ChatMessageState>("\"原有问题\"")!;
Check(legacyText.Content == "原有问题" && legacyText.Role == "user" && legacyText.Status == "completed", "Legacy plain text failed");
var legacyObject = JsonSerializer.Deserialize<ChatMessageState>("{\"Role\":\"assistant\",\"Content\":\"旧回复\"}")!;
Check(legacyObject.Status == "completed" && legacyObject.Reasoning == "" && legacyObject.ReasoningDurationMs == 0, "Legacy object defaults failed");
var saved = new ChatMessageState
{
    Id = requestId, Role = "assistant", Content = "最终回答", Reasoning = "公开的思考摘要",
    Status = "interrupted", Error = "已停止", Provider = "test-provider", Model = "test-model",
    ReasoningDurationMs = 1250, CreatedAt = createdAt
};
var restored = JsonSerializer.Deserialize<ChatMessageState>(JsonSerializer.Serialize(saved))!;
Check(restored.Id == saved.Id && restored.Content == saved.Content && restored.Reasoning == saved.Reasoning &&
    restored.Status == saved.Status && restored.Error == saved.Error && restored.Provider == saved.Provider &&
    restored.Model == saved.Model && restored.ReasoningDurationMs == saved.ReasoningDurationMs && restored.CreatedAt == saved.CreatedAt,
    "Streamed message persistence failed");
saved.Status = "streaming";
restored = JsonSerializer.Deserialize<ChatMessageState>(JsonSerializer.Serialize(saved))!;
Check(restored.Status == "interrupted" && restored.Content == saved.Content && restored.Reasoning == saved.Reasoning, "Crashed generation was not recovered");

// A local fake gateway verifies the real HTTP transport never waits for the whole response.
using var portReservation = new TcpListener(IPAddress.Loopback, 0);
portReservation.Start();
int port = ((IPEndPoint)portReservation.LocalEndpoint).Port;
portReservation.Stop();
using var listener = new HttpListener();
listener.Prefixes.Add($"http://127.0.0.1:{port}/");
listener.Start();
using var serverCancellation = new CancellationTokenSource();
var finishReply = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
var cancelReply = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
var handlers = new List<Task>();
var server = Task.Run(async () =>
{
    try
    {
        while (!serverCancellation.IsCancellationRequested)
        {
            var context = await listener.GetContextAsync().WaitAsync(serverCancellation.Token);
            handlers.Add(Handle(context));
        }
    }
    catch (OperationCanceledException) { }
});
async Task Handle(HttpListenerContext context)
{
    try
    {
        if (context.Request.Url!.AbsolutePath == "/health")
        {
            context.Response.ContentType = "application/json";
            await context.Response.OutputStream.WriteAsync(Encoding.UTF8.GetBytes("{\"status\":\"ok\",\"service\":\"kynxa-model-gateway\"}"));
            return;
        }
        var incoming = await JsonSerializer.DeserializeAsync<ChatRequest>(context.Request.InputStream, json)
            ?? throw new InvalidDataException("Missing request body");
        Check(incoming.RequestId == requestId && incoming.ConversationId == conversationId, "Transport changed request IDs");
        Check(context.Request.Headers["Accept"] == "text/event-stream", "Missing SSE accept header");
        if (incoming.Message == "http-error")
        {
            context.Response.StatusCode = 503;
            context.Response.ContentType = "application/json";
            await context.Response.OutputStream.WriteAsync(Encoding.UTF8.GetBytes("{\"error\":\"fixture unavailable\"}"));
            return;
        }
        context.Response.ContentType = "text/event-stream";
        context.Response.SendChunked = true;
        await context.Response.OutputStream.WriteAsync(Encoding.UTF8.GetBytes(prefix + Frame(Event("text_delta", "立即显示"))));
        await context.Response.OutputStream.FlushAsync();
        if (incoming.Message == "cancel") await cancelReply.Task.WaitAsync(serverCancellation.Token);
        else
        {
            await finishReply.Task.WaitAsync(serverCancellation.Token);
            await context.Response.OutputStream.WriteAsync(Encoding.UTF8.GetBytes(Frame(complete)));
        }
    }
    catch (Exception exception) when (exception is OperationCanceledException or HttpListenerException or IOException) { }
    finally { context.Response.Close(); }
}
string? previousGateway = Environment.GetEnvironmentVariable("KYNXA_MODEL_API_URL");
Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", $"http://127.0.0.1:{port}");
try
{
    using var client = new ModelApiClient();
    using var testDeadline = new CancellationTokenSource(TimeSpan.FromSeconds(5));
    var request = new ChatRequest(conversationId, "live", RequestId: requestId);
    var received = new List<ChatStreamEvent>();
    await foreach (var item in client.StreamReplyAsync(request, testDeadline.Token))
    {
        received.Add(item);
        if (item.Type == "text_delta")
        {
            Check(!finishReply.Task.IsCompleted, "Response was buffered before yielding");
            finishReply.SetResult();
        }
    }
    Check(received.Count == 3 && received[^1].Content == complete.Content, "HTTP SSE reply failed");
    using var stop = new CancellationTokenSource(TimeSpan.FromSeconds(5));
    await ExpectAsync<OperationCanceledException>(async () =>
    {
        await foreach (var item in client.StreamReplyAsync(request with { Message = "cancel" }, stop.Token))
            if (item.Type == "text_delta") stop.Cancel();
    }, "HTTP stream cancellation failed");
    await ExpectAsync<InvalidOperationException>(async () =>
    {
        await foreach (var _ in client.StreamReplyAsync(request with { Message = "http-error" }, testDeadline.Token)) { }
    }, "HTTP error was ignored");
}
finally
{
    Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", previousGateway);
    serverCancellation.Cancel();
    await server;
    await Task.WhenAll(handlers);
    listener.Stop();
}
Console.WriteLine("PASS: fragmented Unicode/multiline SSE, request/order/completion validation, cancellation, legacy and reasoning persistence, crash recovery, real HTTP incremental delivery and error handling.");

sealed class FragmentedStream(byte[] data) : MemoryStream(data)
{
    public override ValueTask<int> ReadAsync(Memory<byte> buffer, CancellationToken cancellationToken = default) =>
        base.ReadAsync(buffer[..Math.Min(buffer.Length, 1)], cancellationToken);
}
sealed class WaitingStream : Stream
{
    public override bool CanRead => true;
    public override bool CanSeek => false;
    public override bool CanWrite => false;
    public override long Length => throw new NotSupportedException();
    public override long Position { get => throw new NotSupportedException(); set => throw new NotSupportedException(); }
    public override async ValueTask<int> ReadAsync(Memory<byte> buffer, CancellationToken cancellationToken = default)
    {
        await Task.Delay(Timeout.InfiniteTimeSpan, cancellationToken);
        return 0;
    }
    public override int Read(byte[] buffer, int offset, int count) => throw new NotSupportedException();
    public override void Flush() => throw new NotSupportedException();
    public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException();
    public override void SetLength(long value) => throw new NotSupportedException();
    public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException();
}
