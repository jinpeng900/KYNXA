using System.Text.Json;
using KYNXA_Desktop.Services;

// Consume a native HTTP fixture through the unchanged desktop reader.
// 通过未修改的桌面解析器读取原生 HTTP 夹具，核验真实消息序列与分段/工具配对。
if (args.Length != 1) throw new ArgumentException("Pass the SSE fixture emitted by gateway_http.");
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
int expectedCalls = mcpDiagnostics > 0 ? 2 : 1;
int expectedPhases = mcpDiagnostics > 0 ? 6 : 4;
if (!completed || calls != expectedCalls || results != expectedCalls || phases != expectedPhases)
    throw new InvalidDataException($"Unexpected native sequence: {events} frames, {calls}/{results} tool pair, {phases} phases.");
Console.WriteLine($"PASS: {events} actual native SSE frames accepted by the desktop; {calls} tool pairs, {phases} segment updates, {mcpDiagnostics} nonterminal MCP diagnostics, final reply intact.");
