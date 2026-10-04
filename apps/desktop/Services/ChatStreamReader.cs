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
        var tools = new Dictionary<string, ToolActivity>(StringComparer.Ordinal);
        var terminalSequences = new Dictionary<string, int>(StringComparer.Ordinal);
        var segments = new Dictionary<string, AssistantSegment>(StringComparer.Ordinal);
        int lastTimelineOrder = -1;
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
                    if (!ChatDurationRules.IsValid(item.DurationMs))
                        throw new InvalidDataException(UiText.Get("模型流返回了无法识别的数据。"));
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
                            if (item.SegmentId is { } segmentId && (!segments.TryGetValue(segmentId, out var active) || active.Status != "streaming"))
                                throw InvalidSegment();
                            if (segments.Count > 0 && item.SegmentId is null) throw InvalidSegment();
                            break;
                        case "assistant_segment":
                            if (item.ToolStreamProtocol != 3 || !AssistantSegmentRules.IsValid(item.Segment))
                                throw InvalidSegment();
                            var segment = item.Segment!;
                            if (segments.TryGetValue(segment.Id, out var previous))
                            {
                                if (previous.Round != segment.Round || previous.Order != segment.Order || previous.Status != "streaming")
                                    throw InvalidSegment();
                            }
                            else
                            {
                                if (segments.Count >= 128 || segment.Order <= lastTimelineOrder || segments.Values.Any(value => value.Round >= segment.Round || value.Status == "streaming" || value.Phase == "final_answer"))
                                    throw InvalidSegment();
                                lastTimelineOrder = segment.Order;
                            }
                            segments[segment.Id] = segment;
                            break;
                        case "tool_call":
                        case "tool_result":
                        case "approval_required":
                            ValidateToolEvent(item);
                            if (segments.Count > 0 && (item.Tool!.Round is null || !segments.Values.Any(value => value.Round == item.Tool.Round && value.Status == "completed" && value.Phase == "commentary" && value.Order < item.Tool.Order)))
                                throw InvalidSegment();
                            string callId = item.Tool!.ToolCallId;
                            string name = item.Tool.Name;
                            if (item.Type == "tool_call")
                            {
                                if (!tools.TryAdd(callId, item.Tool)) throw new InvalidDataException(UiText.Get("工具事件无效。"));
                                if (segments.Count > 0)
                                {
                                    if (item.Tool.Order <= lastTimelineOrder) throw InvalidSegment();
                                    lastTimelineOrder = item.Tool.Order!.Value;
                                }
                            }
                            else if (!tools.TryGetValue(callId, out var original) || original.Name != name || original.Round != item.Tool.Round || original.Order != item.Tool.Order)
                                throw new InvalidDataException(UiText.Get("工具事件无效。"));
                            if (item.Type == "tool_result") { tools.Remove(callId); terminalSequences.Remove(callId); }
                            break;
                        case "terminal_output":
                            if (!HostTerminalOutputRules.IsValid(item.Terminal) ||
                                !tools.TryGetValue(item.Terminal!.ToolCallId, out var terminalCall) || terminalCall.Name != "terminal.host.run" ||
                                item.Terminal.Sequence <= terminalSequences.GetValueOrDefault(item.Terminal.ToolCallId))
                                throw new InvalidDataException(UiText.Get("工具事件无效。"));
                            terminalSequences[item.Terminal.ToolCallId] = item.Terminal.Sequence;
                            break;
                        case "completed":
                            if (item.Content is null) throw new InvalidDataException(UiText.Get("模型流缺少最终回复。"));
                            if (tools.Count > 0) throw new InvalidDataException(UiText.Get("工具事件无效。"));
                            ValidateTerminalSegments(item, segments);
                            terminal = true;
                            break;
                        case "content_snapshot":
                            if (item.Content is null || item.Reasoning is null)
                                throw new InvalidDataException(UiText.Get("模型流缺少最终回复。"));
                            break;
                        case "interrupted":
                        case "error":
                            ValidateTerminalSegments(item, segments);
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
            "tool_result" => (tool.Status is "completed" or "error" or "cancelled" or "unknown") && tool.Result is not null && tool.ApprovalId is null,
            "approval_required" => tool.Status == "approval-required" && tool.ApprovalId is { } id && id != Guid.Empty,
            _ => false
        };
        if (!validStatus) throw new InvalidDataException(UiText.Get("工具事件无效。"));
        if ((tool.Round is null) != (tool.Order is null) || tool.Round is < 1 or > 128 || tool.Order is < 0 or > 1024)
            throw new InvalidDataException(UiText.Get("工具事件无效。"));
        if (tool.ResultRef is { } reference && (reference.Id == Guid.Empty || reference.Bytes < 0 || reference.Bytes > 8388608
            || string.IsNullOrEmpty(reference.Sha256) || reference.Sha256.Length != 64 || reference.Sha256.Any(character => !Uri.IsHexDigit(character))))
            throw new InvalidDataException(UiText.Get("工具事件无效。"));
    }

    private static void ValidateTerminalSegments(ChatStreamEvent item, Dictionary<string, AssistantSegment> known)
    {
        if (item.AssistantSegments is not { } segments)
        {
            if (item.Type == "completed" && known.Count > 0) throw InvalidSegment();
            return;
        }
        if (item.ToolStreamProtocol != 3 || !AssistantSegmentRules.IsValidSequence(segments))
            throw InvalidSegment();
        if (known.Count > 0 && (segments.Length != known.Count || segments.Any(segment => !known.TryGetValue(segment.Id, out var prior) || prior.Round != segment.Round || prior.Order != segment.Order)))
            throw InvalidSegment();
        if (item.Type == "completed" && (segments.Length == 0 || segments.Any(segment => segment.Status != "completed") || segments[^1].Phase != "final_answer"))
            throw InvalidSegment();
    }

    private static InvalidDataException InvalidSegment() => new(UiText.Get("模型流返回的回复阶段无效。"));

}
