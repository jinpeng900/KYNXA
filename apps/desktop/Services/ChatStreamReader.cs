using System.Runtime.CompilerServices;
using System.Text;
using System.Text.Json;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

/// <summary>Reads gateway SSE frames independently of network packet and UTF-8 character boundaries.</summary>
public static class ChatStreamReader
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private const int MaximumEventCharacters = 8 * 1024 * 1024;

    public static async IAsyncEnumerable<ChatStreamEvent> ReadAsync(Stream stream,
        Guid conversationId, Guid requestId, [EnumeratorCancellation] CancellationToken cancellationToken = default)
    {
        using var reader = new StreamReader(stream, new UTF8Encoding(false, true),
            detectEncodingFromByteOrderMarks: true, bufferSize: 4096, leaveOpen: true);
        var data = new StringBuilder();
        bool started = false;
        var tools = new Dictionary<string, string>(StringComparer.Ordinal);
        while (true)
        {
            cancellationToken.ThrowIfCancellationRequested();
            string? line = await reader.ReadLineAsync(cancellationToken);
            if (line is null || line.Length == 0)
            {
                if (data.Length > 0)
                {
                    ChatStreamEvent item;
                    try
                    {
                        item = JsonSerializer.Deserialize<ChatStreamEvent>(data.ToString(), JsonOptions)
                            ?? throw new InvalidDataException(UiText.Get("模型流返回了空事件。"));
                    }
                    catch (JsonException exception)
                    {
                        throw new InvalidDataException(UiText.Get("模型流返回了无法识别的数据。"), exception);
                    }
                    data.Clear();
                    if (item.ConversationId != conversationId || item.RequestId != requestId || item.CreatedAt == default)
                        throw new InvalidDataException(UiText.Get("模型流返回的会话或请求信息不匹配。"));
                    if (!started && item.Type != "started")
                        throw new InvalidDataException(UiText.Get("模型流缺少开始事件。"));
                    bool terminal = false;
                    switch (item.Type)
                    {
                        case "started":
                            if (started) throw new InvalidDataException(UiText.Get("模型流重复开始了同一条回复。"));
                            started = true;
                            break;
                        case "text_delta":
                        case "reasoning_delta":
                            if (item.Delta is null) throw new InvalidDataException(UiText.Get("模型流缺少增量内容。"));
                            break;
                        case "tool_call":
                        case "tool_result":
                        case "approval_required":
                            ValidateToolEvent(item);
                            string callId = item.Tool!.ToolCallId;
                            string name = item.Tool.Name;
                            if (item.Type == "tool_call")
                            {
                                if (!tools.TryAdd(callId, name)) throw new InvalidDataException(UiText.Get("工具事件无效。"));
                            }
                            else if (!tools.TryGetValue(callId, out string? originalName) || originalName != name)
                                throw new InvalidDataException(UiText.Get("工具事件无效。"));
                            if (item.Type == "tool_result") tools.Remove(callId);
                            break;
                        case "completed":
                            if (item.Content is null) throw new InvalidDataException(UiText.Get("模型流缺少最终回复。"));
                            terminal = true;
                            break;
                        case "content_snapshot":
                            if (item.Content is null || item.Reasoning is null)
                                throw new InvalidDataException(UiText.Get("模型流缺少最终回复。"));
                            break;
                        case "interrupted":
                        case "error":
                            terminal = true;
                            break;
                        default:
                            throw new InvalidDataException(UiText.Get("模型流返回了未知的事件类型。"));
                    }
                    yield return item;
                    if (terminal) yield break;
                }
                if (line is null) throw new EndOfStreamException(UiText.Get("模型连接已断开，已保留收到的内容。"));
                continue;
            }
            // Ignore comments/heartbeats and optional SSE event/id/retry fields.
            int separator = line.IndexOf(':');
            string field = separator < 0 ? line : line[..separator];
            if (field != "data") continue;
            string value = separator < 0 ? string.Empty : line[(separator + 1)..];
            if (value.StartsWith(' ')) value = value[1..];
            if (data.Length + value.Length + 1 > MaximumEventCharacters)
                throw new InvalidDataException(UiText.Get("模型流的单条事件过大。"));
            data.Append(value).Append('\n');
        }
    }
    private static void ValidateToolEvent(ChatStreamEvent item)
    {
        ToolActivity? tool = item.Tool;
        if (tool is null || string.IsNullOrWhiteSpace(tool.ToolCallId) || tool.ToolCallId.Length > 200
            || string.IsNullOrWhiteSpace(tool.Name) || tool.Name.Length > 200 || tool.Arguments is not { ValueKind: JsonValueKind.Object }
            || tool.Arguments.Value.GetRawText().Length > 65536
            || string.IsNullOrWhiteSpace(tool.Summary) || tool.Summary.Length > 4096
            || tool.Result?.Length > 65536)
            throw new InvalidDataException(UiText.Get("工具事件无效。"));
        bool validStatus = item.Type switch
        {
            "tool_call" => tool.Status == "running" && tool.ApprovalId is null,
            "tool_result" => (tool.Status is "completed" or "error") && tool.Result is not null && tool.ApprovalId is null,
            "approval_required" => tool.Status == "approval-required" && tool.ApprovalId is { } id && id != Guid.Empty,
            _ => false
        };
        if (!validStatus) throw new InvalidDataException(UiText.Get("工具事件无效。"));
    }

}
