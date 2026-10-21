using System.Runtime.CompilerServices;
using System.Text;
using System.Text.Json;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Reads gateway SSE frames independently of network packet and UTF-8 character boundaries.
/// 读取网关 SSE 帧，不依赖网络包边界或 UTF-8 字符的分包位置。
/// </summary>
public static class ChatStreamReader
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private const int MaximumEventCharacters = 8 * 1024 * 1024;
    // Large document writes keep their complete approval parameters within the separately bounded SSE frame.
    // 长文写入保留完整审批参数；整体 SSE 帧仍有独立的有界保护。
    private const int MaximumToolArgumentCharacters = 1024 * 1024;

    public static async IAsyncEnumerable<ChatStreamEvent> ReadAsync(Stream stream,
        Guid conversationId, Guid requestId, [EnumeratorCancellation] CancellationToken cancellationToken = default)
    {
        using var reader = new StreamReader(stream, new UTF8Encoding(false, true),
            detectEncodingFromByteOrderMarks: true, bufferSize: 4096, leaveOpen: true);
        var eventData = new StringBuilder();
        bool hasStarted = false;
        Guid? rootTaskId = null;
        var activeToolCalls = new Dictionary<string, ToolActivity>(StringComparer.Ordinal);
        var terminalOutputSequences = new Dictionary<string, int>(StringComparer.Ordinal);
        var assistantSegments = new Dictionary<string, AssistantSegment>(StringComparer.Ordinal);
        int lastTimelineOrder = -1;
        while (true)
        {
            cancellationToken.ThrowIfCancellationRequested();
            string? line = await reader.ReadLineAsync(cancellationToken);
            if (line is null || line.Length == 0)
            {
                if (eventData.Length > 0)
                {
                    ChatStreamEvent streamEvent;
                    try
                    {
                        streamEvent = JsonSerializer.Deserialize<ChatStreamEvent>(eventData.ToString(), JsonOptions)
                            ?? throw new InvalidDataException(UiText.Get("模型流返回了空事件。"));
                    }
                    catch (JsonException exception)
                    {
                        throw new InvalidDataException(UiText.Get("模型流返回了无法识别的数据。"), exception);
                    }
                    eventData.Clear();
                    if (streamEvent.ConversationId != conversationId || streamEvent.RequestId != requestId || streamEvent.CreatedAt == default)
                        throw new InvalidDataException(UiText.Get("模型流返回的会话或请求信息不匹配。"));
                    if (!ChatDurationRules.IsValid(streamEvent.DurationMs))
                        throw new InvalidDataException(UiText.Get("模型流返回了无法识别的数据。"));
                    // Optional display timing must never terminate an otherwise valid reply stream.
                    // 可选展示计时缺失或越界时忽略，不因此结束有效回复，也不改写正式结算用时。
                    if (streamEvent.GenerationElapsedMs is not long elapsed || !ChatDurationRules.IsValid(elapsed)
                        || streamEvent.GenerationActive is null)
                        streamEvent = streamEvent with { GenerationElapsedMs = null, GenerationActive = null };
                    if (!hasStarted && streamEvent.Type != "started")
                        throw new InvalidDataException(UiText.Get("模型流缺少开始事件。"));
                    bool isTerminalEvent = false;
                    switch (streamEvent.Type)
                    {
                        case "started":
                            if (hasStarted) throw new InvalidDataException(UiText.Get("模型流重复开始了同一条回复。"));
                            hasStarted = true;
                            if (streamEvent.TaskId == Guid.Empty || streamEvent.RootRequestTaskId == Guid.Empty)
                                throw new InvalidDataException(UiText.Get("任务事件无效。"));
                            rootTaskId = streamEvent.RootRequestTaskId ?? streamEvent.TaskId;
                            break;
                        case "generation_timing":
                            if (streamEvent.GenerationElapsedMs is null) continue;
                            break;
                        case "reply_anchor":
                            if (streamEvent.ReplyMessageId is not Guid replyMessageId || replyMessageId == Guid.Empty
                                || streamEvent.UserMessageId is not Guid userMessageId || userMessageId == Guid.Empty
                                || streamEvent.SupersededReplyMessageId == Guid.Empty)
                                throw new InvalidDataException(UiText.Get("模型流返回的会话或请求信息不匹配。"));
                            // A steering boundary starts a new visible reply while preserving the task and tool receipts.
                            // 引导边界开启新的可见回复，任务身份及工具回执继续沿用，不重开整个请求。
                            assistantSegments.Clear();
                            lastTimelineOrder = -1;
                            break;
                        case "text_delta":
                        case "reasoning_delta":
                            if (streamEvent.Delta is null) throw new InvalidDataException(UiText.Get("模型流缺少增量内容。"));
                            if (streamEvent.SegmentId is { } segmentId && (!assistantSegments.TryGetValue(segmentId, out var active) || active.Status != "streaming"))
                                throw InvalidSegment();
                            if (assistantSegments.Count > 0 && streamEvent.SegmentId is null) throw InvalidSegment();
                            break;
                        case "assistant_segment":
                            if (streamEvent.ToolStreamProtocol != 3 || !AssistantSegmentRules.IsValid(streamEvent.Segment))
                                throw InvalidSegment();
                            var segment = streamEvent.Segment!;
                            if (assistantSegments.TryGetValue(segment.Id, out var previous))
                            {
                                if (!IsValidSegmentUpdate(previous, segment))
                                    throw InvalidSegment();
                            }
                            else
                            {
                                if (assistantSegments.Count >= 512 || segment.Order <= lastTimelineOrder || assistantSegments.Values.Any(value => value.Round >= segment.Round || value.Status == "streaming" || value.Phase == "final_answer"))
                                    throw InvalidSegment();
                                lastTimelineOrder = segment.Order;
                            }
                            assistantSegments[segment.Id] = segment;
                            break;
                        case "tool_call":
                        case "tool_result":
                        case "approval_required":
                            ValidateToolEvent(streamEvent);
                            // Context maintenance is display-only, including recovery after the latest interrupted step.
                            // 上下文维护只是展示活动，也可以接在最新中断步骤后恢复；真实工具仍须等待该轮输出完成。
                            bool isContextActivity = streamEvent.Tool!.Name == "context.compact";
                            if (isContextActivity && (streamEvent.Type == "approval_required" ||
                                streamEvent.Tool.Arguments!.Value.EnumerateObject().Any()))
                                throw new InvalidDataException(UiText.Get("工具事件无效。"));
                            if (assistantSegments.Count > 0 && (streamEvent.Tool.Round is null || !assistantSegments.Values.Any(value =>
                                value.Round == streamEvent.Tool.Round && value.Phase == "commentary" && value.Order < streamEvent.Tool.Order &&
                                (value.Status == "completed" || isContextActivity && (value.Status == "streaming" ||
                                    value.Status == "interrupted" && value.Round == assistantSegments.Values.Max(known => known.Round))))))
                                throw InvalidSegment();
                            string toolCallId = streamEvent.Tool!.ToolCallId;
                            string toolName = streamEvent.Tool.Name;
                            if (streamEvent.Type == "tool_call")
                            {
                                if (!activeToolCalls.TryAdd(toolCallId, streamEvent.Tool)) throw new InvalidDataException(UiText.Get("工具事件无效。"));
                                if (assistantSegments.Count > 0)
                                {
                                    if (streamEvent.Tool.Order <= lastTimelineOrder) throw InvalidSegment();
                                    lastTimelineOrder = streamEvent.Tool.Order!.Value;
                                }
                            }
                            else if (!activeToolCalls.TryGetValue(toolCallId, out var original) || original.Name != toolName || original.Round != streamEvent.Tool.Round || original.Order != streamEvent.Tool.Order)
                                throw new InvalidDataException(UiText.Get("工具事件无效。"));
                            if (streamEvent.Type == "tool_result") { activeToolCalls.Remove(toolCallId); terminalOutputSequences.Remove(toolCallId); }
                            break;
                        case "approval_closed":
                            // Closing an approval changes neither the active tool nor the reply's terminal state.
                            // 审批关闭不结束工具或回复；后续真实工具回执仍必须正常接收。
                            if (streamEvent.ApprovalId is not Guid closedId || closedId == Guid.Empty ||
                                string.IsNullOrWhiteSpace(streamEvent.ToolCallId) || streamEvent.ToolCallId.Length > 200 ||
                                streamEvent.TaskId == Guid.Empty || streamEvent.Scope is not (null or "once" or "task"))
                                throw new InvalidDataException(UiText.Get("工具事件无效。"));
                            break;
                        case "terminal_output":
                            if (!HostTerminalOutputRules.IsValid(streamEvent.Terminal) ||
                                !activeToolCalls.TryGetValue(streamEvent.Terminal!.ToolCallId, out var terminalCall) || terminalCall.Name != "terminal.host.run" ||
                                streamEvent.Terminal.Sequence <= terminalOutputSequences.GetValueOrDefault(streamEvent.Terminal.ToolCallId))
                                throw new InvalidDataException(UiText.Get("工具事件无效。"));
                            terminalOutputSequences[streamEvent.Terminal.ToolCallId] = streamEvent.Terminal.Sequence;
                            break;
                        case "workspace_terminal_open":
                            // The gateway already owns this PTY; the event attaches its renderer, never creates a second session.
                            // 网关已拥有此伪终端；事件只挂接渲染视图，不能再建立第二个会话。
                            if (string.IsNullOrWhiteSpace(streamEvent.SessionId) || streamEvent.SessionId.Length > 200 ||
                                streamEvent.SessionId.Any(char.IsControl) || streamEvent.Cols is not (>= 2 and <= 500) ||
                                streamEvent.Rows is not (>= 1 and <= 200))
                                throw new InvalidDataException(UiText.Get("工具事件无效。"));
                            break;
                        case "mcp_status":
                            // A service discovery diagnostic does not settle the model reply or its active tools.
                            // 服务发现诊断不结束模型回复，也不改变当前工具状态；会话身份仍经过上面的统一校验。
                            if (string.IsNullOrWhiteSpace(streamEvent.ServerId) || streamEvent.ServerId.Length > 80 ||
                                streamEvent.Ready is null || string.IsNullOrWhiteSpace(streamEvent.Code) || streamEvent.Code.Length > 200 ||
                                string.IsNullOrWhiteSpace(streamEvent.Message) || streamEvent.Message.Length > 65536)
                                throw new InvalidDataException(UiText.Get("工具事件无效。"));
                            break;
                        case "task_progress":
                            if (string.IsNullOrWhiteSpace(streamEvent.Phase) || streamEvent.Phase.Length > 80 || streamEvent.Round is < 1)
                                throw new InvalidDataException(UiText.Get("任务事件无效。"));
                            goto case "task_tool_call";
                        case "task_tool_call":
                        case "task_tool_result":
                        case "task_finished":
                        case "task_approval_closed":
                            // Child diagnostics are not main-agent tools or terminal reply events.
                            // 子任务诊断不加入主代理工具账本，也不结束主回复；正式回执通过任务 API 读取。
                            if (streamEvent.TaskId is not Guid task || task == Guid.Empty || streamEvent.RootRequestTaskId == Guid.Empty
                                || (rootTaskId is Guid root && streamEvent.RootRequestTaskId is Guid actual && root != actual))
                                throw new InvalidDataException(UiText.Get("任务事件无效。"));
                            if (streamEvent.Type == "task_approval_closed" && (streamEvent.ApprovalId is not Guid closedApproval || closedApproval == Guid.Empty))
                                throw new InvalidDataException(UiText.Get("任务事件无效。"));
                            break;
                        case "task_approval_required":
                            if (streamEvent.TaskId is not Guid approvalTask || approvalTask == Guid.Empty
                                || streamEvent.RootRequestTaskId == Guid.Empty
                                || (rootTaskId is Guid approvalRoot && streamEvent.RootRequestTaskId is Guid actualRoot && approvalRoot != actualRoot))
                                throw new InvalidDataException(UiText.Get("任务事件无效。"));
                            if (streamEvent.Tool is { } childTool && (childTool.Round is < 1 || childTool.Order is < 0
                                || (childTool.Round is null) != (childTool.Order is null)))
                                throw new InvalidDataException(UiText.Get("工具事件无效。"));
                            ValidateToolEvent(streamEvent with { Type = "approval_required",
                                Tool = streamEvent.Tool is { } approvalTool ? approvalTool with { Round = null, Order = null } : null });
                            break;
                        case "completed":
                            if (streamEvent.Content is null) throw new InvalidDataException(UiText.Get("模型流缺少最终回复。"));
                            if (activeToolCalls.Count > 0) throw new InvalidDataException(UiText.Get("工具事件无效。"));
                            ValidateTerminalSegments(streamEvent, assistantSegments);
                            isTerminalEvent = true;
                            break;
                        case "content_snapshot":
                            if (streamEvent.Content is null || streamEvent.Reasoning is null)
                                throw new InvalidDataException(UiText.Get("模型流缺少最终回复。"));
                            break;
                        case "interrupted":
                        case "error":
                            ValidateTerminalSegments(streamEvent, assistantSegments);
                            isTerminalEvent = true;
                            break;
                        default:
                            throw new InvalidDataException(UiText.Get("模型流返回了未知的事件类型。"));
                    }
                    yield return streamEvent;
                    if (isTerminalEvent) yield break;
                }
                if (line is null) throw new EndOfStreamException(UiText.Get("模型连接已断开，已保留收到的内容。"));
                continue;
            }
            // Ignore comments/heartbeats and optional SSE event/id/retry fields.
            // 忽略注释、心跳与可选的 SSE event、id、retry 字段。
            int separator = line.IndexOf(':');
            string field = separator < 0 ? line : line[..separator];
            if (field != "data") continue;
            string value = separator < 0 ? string.Empty : line[(separator + 1)..];
            if (value.StartsWith(' ')) value = value[1..];
            if (eventData.Length + value.Length + 1 > MaximumEventCharacters)
                throw new InvalidDataException(UiText.Get("模型流的单条事件过大。"));
            eventData.Append(value).Append('\n');
        }
    }
    private static void ValidateToolEvent(ChatStreamEvent streamEvent)
    {
        ToolActivity? tool = streamEvent.Tool;
        if (tool is null || string.IsNullOrWhiteSpace(tool.ToolCallId) || tool.ToolCallId.Length > 200
            || string.IsNullOrWhiteSpace(tool.Name) || tool.Name.Length > 200 || tool.Arguments is not { ValueKind: JsonValueKind.Object }
            || tool.Arguments.Value.GetRawText().Length > MaximumToolArgumentCharacters
            || string.IsNullOrWhiteSpace(tool.Summary) || tool.Summary.Length > 4096
            || tool.Result?.Length > 65536 || tool.TaskApprovalDescription?.Length > 2048)
            throw new InvalidDataException(UiText.Get("工具事件无效。"));
        bool validStatus = streamEvent.Type switch
        {
            "tool_call" => tool.Status == "running" && tool.ApprovalId is null,
            "tool_result" => (tool.Status is "completed" or "error" or "cancelled" or "unknown") && tool.Result is not null && tool.ApprovalId is null,
            "approval_required" => tool.Status == "approval-required" && tool.ApprovalId is { } id && id != Guid.Empty,
            _ => false
        };
        if (!validStatus) throw new InvalidDataException(UiText.Get("工具事件无效。"));
        if ((tool.Round is null) != (tool.Order is null) || tool.Round is < 1 or > 512 || tool.Order is < 0 or > 8192)
            throw new InvalidDataException(UiText.Get("工具事件无效。"));
        if (tool.ResultRef is { } reference && (reference.Id == Guid.Empty || reference.Bytes < 0 || reference.Bytes > 8388608
            || string.IsNullOrEmpty(reference.Sha256) || reference.Sha256.Length != 64 || reference.Sha256.Any(character => !Uri.IsHexDigit(character))))
            throw new InvalidDataException(UiText.Get("工具事件无效。"));
    }

    private static bool IsValidSegmentUpdate(AssistantSegment previous, AssistantSegment segment)
    {
        if (previous.Id != segment.Id || previous.Round != segment.Round || previous.Order != segment.Order)
            return false;
        if (previous.Phase == "final_answer" && segment.Phase != "final_answer") return false;
        if (previous.Status == "streaming") return true;

        // Closing a round is idempotent. Later classification may promote its phase or mark an answer continuation.
        // 同轮结束事件可幂等重放；后续分类可确定最终阶段或正文续写标记，不得借此改写已关闭的正文与摘要。
        return previous.Status == segment.Status && previous.Content == segment.Content &&
            previous.Reasoning == segment.Reasoning && previous.ReasoningDurationMs <= segment.ReasoningDurationMs &&
            (!previous.IsAnswerContinuation || segment.IsAnswerContinuation || segment.Phase == "final_answer");
    }

    private static void ValidateTerminalSegments(ChatStreamEvent streamEvent, Dictionary<string, AssistantSegment> knownSegments)
    {
        if (streamEvent.AssistantSegments is not { } assistantSegments)
        {
            if (streamEvent.Type == "completed" && knownSegments.Count > 0) throw InvalidSegment();
            return;
        }
        if (streamEvent.ToolStreamProtocol != 3 || !AssistantSegmentRules.IsValidSequence(assistantSegments))
            throw InvalidSegment();
        if (knownSegments.Count > 0 && (assistantSegments.Length != knownSegments.Count || assistantSegments.Any(segment =>
            !knownSegments.TryGetValue(segment.Id, out var prior) || !IsValidSegmentUpdate(prior, segment))))
            throw InvalidSegment();
        // An interrupted model step may recover into a complete final answer; it remains a truthful commentary trace.
        // 中断的模型步骤可以恢复并得到完整最终回答；先前中断段仍保留为真实的过程轨迹。
        if (streamEvent.Type == "completed" && (assistantSegments.Length == 0 || assistantSegments.Any(segment =>
            segment.Status != "completed" && !(segment.Phase == "commentary" && segment.Status == "interrupted")) ||
            assistantSegments[^1].Phase != "final_answer" || assistantSegments[^1].Status != "completed"))
            throw InvalidSegment();
    }

    private static InvalidDataException InvalidSegment() => new(UiText.Get("模型流返回的回复阶段无效。"));

}
