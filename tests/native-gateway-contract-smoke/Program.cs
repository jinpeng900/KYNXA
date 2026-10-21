using System.Text.Json;
using KYNXA_Desktop.Services;

// Consume a native HTTP fixture through the unchanged desktop reader.
// 通过未修改的桌面解析器读取原生 HTTP 夹具，核验真实消息序列与分段/工具配对。
if (args.Length is < 1 or > 2) throw new ArgumentException("Pass the SSE fixture emitted by gateway_http and optionally its expected tool-pair count.");
string fixture = Path.GetFullPath(args[0]);
string first = File.ReadLines(fixture).First(line => line.StartsWith("data: ", StringComparison.Ordinal))[6..];
using var identity = JsonDocument.Parse(first);
Guid conversation = identity.RootElement.GetProperty("conversationId").GetGuid();
Guid request = identity.RootElement.GetProperty("requestId").GetGuid();
using var stream = File.OpenRead(fixture);
int events = 0, calls = 0, results = 0, phases = 0, mcpDiagnostics = 0;
bool completed = false;
await foreach (var frame in ChatStreamReader.ReadAsync(stream, conversation, request))
{
    events++;
    if (frame.Type == "tool_call") calls++;
    if (frame.Type == "tool_result") results++;
    if (frame.Type == "assistant_segment") phases++;
    if (frame.Type == "mcp_status")
    {
        if (frame.ServerId != "offline-fixture" || frame.Ready != false || string.IsNullOrWhiteSpace(frame.Code) || string.IsNullOrWhiteSpace(frame.Message))
            throw new InvalidDataException("The actual MCP diagnostic lost its service identity or outcome.");
        mcpDiagnostics++;
    }
    if (frame.Type == "completed")
    {
        if (frame.Content?.Contains(mcpDiagnostics > 0 ? "仍能继续" : "native-file-body", StringComparison.Ordinal) != true)
            throw new InvalidDataException("The actual file evidence was lost before the final answer.");
        completed = true;
    }
}
int expectedCalls = args.Length == 2 ? int.Parse(args[1], System.Globalization.CultureInfo.InvariantCulture) : mcpDiagnostics > 0 ? 2 : 1;
// Buffered commentary can arrive already completed; the production reader validates its full lifecycle.
// 缓冲进度可直接发布已结束的分段；完整生命周期仍由生产解析器逐事件校验，不能要求空生成事件凑固定数量。
if (!completed || calls != expectedCalls || results != expectedCalls || phases < 2)
    throw new InvalidDataException($"Unexpected native sequence: {events} frames, {calls}/{results} tool pair, {phases} phases.");
Console.WriteLine($"PASS: {events} actual native SSE frames accepted by the desktop; {calls} tool pairs, {phases} segment updates, {mcpDiagnostics} nonterminal MCP diagnostics, final reply intact.");
