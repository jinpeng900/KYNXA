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
Guid conversationId = Guid.NewGuid(), requestId = Guid.NewGuid(), userMessageId = Guid.NewGuid();
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
var complete = Event("completed", content: "你好😀\n最终 **答案**", reasoning: "先核对 $1+1=2$。") with { DurationMs = 6789 };
string pretty = JsonSerializer.Serialize(complete, new JsonSerializerOptions(json) { WriteIndented = true });
string multilineFrame = string.Join('\n', pretty.Split('\n').Select(line => "data: " + line)) + "\n\n";
var result = await Read(prefix + Frame(Event("reasoning_delta", "先核对 $1+1=2$。")) +
    Frame(Event("text_delta", "你好😀\n")) + Frame(Event("text_delta", "草稿")) + multilineFrame);
Check(result.Count == 5 && result[1].Delta!.Contains("$1+1=2$") && result[2].Delta == "你好😀\n", "Unicode fragmentation or multiline SSE failed");
Check(result[^1].Content == complete.Content, "Final canonical content was lost");
Check(result[^1].DurationMs == 6789, "Gateway generation duration was lost");
foreach (string state in new[] { "completed", "partial", "verification-failed", "execution-unknown" })
{
    var completion = JsonSerializer.SerializeToElement(new { completionState = state, state = "needs-validation", correctnessCertified = false });
    var received = (await Read(prefix + Frame(complete with { TaskCompletion = completion })))[^1];
    Check(received.Type == "completed" && ChatMessageState.ReadTaskCompletion(received.TaskCompletion) == state,
        "SSE transport and task outcome dimensions were not retained independently.");
}
Check((await Read(prefix + Frame(Event("completed", content: "legacy"))))[^1].DurationMs == 0, "Old duration default changed");
foreach (long invalidDuration in new[] { -1L, ChatDurationRules.MaximumDurationMs + 1 })
    await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(complete with { DurationMs = invalidDuration })), "Invalid stream duration accepted");
Check((await Read(prefix + Frame(Event("error", content: "已收到", reasoning: "思考", error: "连接中断"))))[^1].Error == "连接中断", "Error event lost partial content");
Check((await Read(prefix + Frame(Event("interrupted", content: "部分"))))[^1].Type == "interrupted", "Interrupted terminal not recognized");
await ExpectAsync<EndOfStreamException>(async () => { await Read(prefix + Frame(Event("text_delta", "部分"))); }, "Premature EOF accepted");
await ExpectAsync<InvalidDataException>(async () => { await Read(Frame(Event("text_delta", "无开始事件"))); }, "Missing start accepted");
await ExpectAsync<InvalidDataException>(async () => { await Read(prefix + Frame(Event("started"))); }, "Duplicate start accepted");
await ExpectAsync<InvalidDataException>(async () => { await Read(prefix + Frame(Event("text_delta", "其他会话") with { ConversationId = Guid.NewGuid() })); }, "Wrong conversation accepted");
await ExpectAsync<InvalidDataException>(async () => { await Read(prefix + Frame(Event("text_delta", "其他请求") with { RequestId = Guid.NewGuid() })); }, "Wrong request accepted");
await ExpectAsync<InvalidDataException>(async () => { await Read(prefix + Frame(Event("completed"))); }, "Missing final content accepted");
await ExpectAsync<InvalidDataException>(async () => { await Read(prefix + "data: {broken}\n\n"); }, "Malformed JSON accepted");
var unavailableMcp = Event("mcp_status") with
{
    ServerId = "offline-fixture", Ready = false, Code = "MCP_CONNECT_FAILED", Message = "本机隔离 MCP 不可用，继续现有任务。"
};
var mcpContinued = await Read(prefix + Frame(unavailableMcp) + Frame(Event("text_delta", "仍能继续回答。"))
    + Frame(Event("completed", content: "仍能继续回答。")));
Check(mcpContinued.Count == 4 && mcpContinued[1].ServerId == "offline-fixture" && mcpContinued[1].Ready == false
    && mcpContinued[^1].Content == "仍能继续回答。", "A nonterminal MCP status interrupted the reply or lost its diagnostic.");
foreach (var malformed in new[] { unavailableMcp with { ServerId = null }, unavailableMcp with { Ready = null },
    unavailableMcp with { Code = null }, unavailableMcp with { Message = null }, unavailableMcp with { ServerId = new string('x', 81) },
    unavailableMcp with { Code = new string('x', 201) }, unavailableMcp with { Message = new string('x', 65537) } })
    await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(malformed)), "Invalid MCP diagnostic accepted.");
await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(unavailableMcp with { ConversationId = Guid.NewGuid() })),
    "MCP status bypassed conversation identity.");
await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(Event("unregistered_status"))), "Unregistered event accepted.");
// Child events are a separate task projection, not replayed main-agent tools or final replies.
// 子任务事件单独投影，不重放为主代理工具，也不能提前结束主回复。
Guid rootTaskId = Guid.NewGuid(), childTaskId = Guid.NewGuid();
string taskPrefix = Frame(Event("started") with { TaskId = rootTaskId, RootRequestTaskId = rootTaskId });
var childEvents = new[]
{
    Event("task_progress") with { TaskId = childTaskId, RootRequestTaskId = rootTaskId, Phase = "model", Round = 1 },
    Event("task_tool_call") with { TaskId = childTaskId, RootRequestTaskId = rootTaskId },
    Event("task_tool_result") with { TaskId = childTaskId, RootRequestTaskId = rootTaskId },
    Event("task_finished") with { TaskId = childTaskId, RootRequestTaskId = rootTaskId }
};
var childContinued = await Read(taskPrefix + string.Concat(childEvents.Select(Frame))
    + Frame(Event("text_delta", "结合子任务结果继续回答")) + Frame(Event("completed", content: "最终回答")));
Check(childContinued.Count == 7 && childContinued[^1].Type == "completed" && childContinued[^1].Content == "最终回答",
    "Child diagnostics interrupted or prematurely completed the main response.");
await ExpectAsync<InvalidDataException>(() => Read(taskPrefix + Frame(childEvents[0] with { TaskId = Guid.Empty })), "Empty child task identity accepted.");
await ExpectAsync<InvalidDataException>(() => Read(taskPrefix + Frame(childEvents[0] with { RootRequestTaskId = Guid.NewGuid() })), "Child progress crossed a root task identity.");
await ExpectAsync<InvalidDataException>(() => Read(taskPrefix + Frame(childEvents[0] with { ConversationId = Guid.NewGuid() })), "Child progress bypassed chat identity.");
await ExpectAsync<InvalidDataException>(() => Read(taskPrefix + Frame(childEvents[0] with { Round = -1 })), "Negative child round accepted.");
var childApprovalId = Guid.NewGuid();
var childApproval = Event("task_approval_required") with { TaskId = childTaskId, RootRequestTaskId = rootTaskId,
    Tool = new ToolActivity("child_call", "filesystem.write", JsonSerializer.SerializeToElement(new { path = "guide.md" }),
        "approval-required", "写入授权工作文件", ApprovalId: childApprovalId, Round: 256, Order: 2048) };
var childClosed = Event("task_approval_closed") with { TaskId = childTaskId, RootRequestTaskId = rootTaskId, ApprovalId = childApprovalId };
var childApprovalFlow = await Read(taskPrefix + Frame(childApproval) + Frame(childClosed) + Frame(Event("completed", content: "子审批已撤销，继续总结。")));
Check(childApprovalFlow.Count == 4 && childApprovalFlow[^1].Type == "completed",
    "Independent child approval contaminated the main timeline or interrupted its response.");
await ExpectAsync<InvalidDataException>(() => Read(taskPrefix + Frame(childClosed with { ApprovalId = null })), "Missing closed approval identity accepted.");
await ExpectAsync<InvalidDataException>(() => Read(taskPrefix + Frame(childApproval with { Tool = childApproval.Tool! with { ApprovalId = Guid.Empty } })),
    "Invalid child approval receipt accepted.");
using (var cancellation = new CancellationTokenSource(30))
using (var stream = new WaitingStream())
{
    await ExpectAsync<OperationCanceledException>(async () =>
    {
        await foreach (var _ in ChatStreamReader.ReadAsync(stream, conversationId, requestId, cancellation.Token)) { }
    }, "Cancellation did not interrupt a pending read");
}

using (var argsJson = JsonDocument.Parse("{\"path\":\"hello.txt\"}"))
{
    var tool = new ToolActivity("call_1", "filesystem.write", argsJson.RootElement.Clone(), "running", "Write hello.txt");
    ChatStreamEvent ToolEvent(string type, ToolActivity value) => Event(type) with { Tool = value };
    var approval = tool with { Status = "approval-required", ApprovalId = Guid.NewGuid() };
    var reference = new ToolResultReference(Guid.NewGuid(), 120, new string('a', 64));
    var toolResult = tool with { Status = "completed", Result = "{\"written\":true}", ResultRef = reference };
    var toolEvents = await Read(prefix + Frame(ToolEvent("tool_call", tool)) + Frame(ToolEvent("approval_required", approval))
        + Frame(ToolEvent("tool_result", toolResult)) + Frame(Event("completed", content: "done")));
    Check(toolEvents.Count == 5 && toolEvents[2].Tool?.ApprovalId == approval.ApprovalId, "Tool/approval SSE lost identity");
    Check(toolEvents[3].Tool?.ResultRef == reference, "Completed result reference was lost");
    foreach (string status in new[] { "unknown", "cancelled" })
        Check((await Read(prefix + Frame(ToolEvent("tool_call", tool))
            + Frame(ToolEvent("tool_result", toolResult with { Status = status }))
            + Frame(Event("interrupted", content: "stopped"))))[2].Tool?.Status == status,
            "Interrupted tool outcome was rejected");
    foreach (var invalid in new[] { reference with { Id = Guid.Empty }, reference with { Bytes = 8388609 },
        reference with { Sha256 = "invalid" }, reference with { Sha256 = null! } })
        await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(ToolEvent("tool_call", tool))
            + Frame(ToolEvent("tool_result", toolResult with { ResultRef = invalid }))), "Invalid result reference accepted");
    await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(ToolEvent("approval_required", approval))), "Unannounced approval accepted");
    await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(ToolEvent("tool_call", tool))
        + Frame(ToolEvent("tool_result", toolResult with { Name = "other" }))), "Tool result changed operation");
    await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(ToolEvent("tool_call", tool))
        + Frame(ToolEvent("tool_call", tool))), "Duplicate tool call accepted");
    await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(ToolEvent("tool_call", tool))
        + Frame(ToolEvent("approval_required", approval with { ApprovalId = Guid.Empty }))), "Empty approval ID accepted");
}

Check((await Read(prefix + Frame(Event("content_snapshot", content: "修订正文", reasoning: "修订摘要"))
    + Frame(Event("interrupted", content: "修订正文"))))[1].Content == "修订正文", "Round snapshot was not accepted");
await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(Event("content_snapshot", content: "缺摘要"))), "Incomplete snapshot accepted");

var hostCall = new ToolActivity("host-live-fixture", "terminal.host.run", JsonSerializer.SerializeToElement(new { shell = "cmd", script = "echo fixture" }), "running", "Run command");
var hostOutput = new HostTerminalOutput(hostCall.ToolCallId, 1, "stdout", "实时输出😀\n");
ChatStreamEvent HostOutput(HostTerminalOutput output) => Event("terminal_output") with { Terminal = output };
string hostStart = prefix + Frame(Event("tool_call") with { Tool = hostCall });
var hostEvents = await Read(hostStart + Frame(HostOutput(hostOutput)) + Frame(HostOutput(hostOutput with { Sequence = 2, Stream = "stderr", Text = "错误预览" })) +
    Frame(Event("tool_result") with { Tool = hostCall with { Status = "completed", Result = "fixture receipt" } }) + Frame(Event("completed", content: "done")));
Check(hostEvents[2].Terminal?.Text == hostOutput.Text && hostEvents[3].Terminal?.Sequence == 2, "Live terminal Unicode and sequence was lost");
foreach (var invalid in new[] { hostOutput with { ToolCallId = "other-call" }, hostOutput with { Sequence = 0 },
    hostOutput with { Stream = "unknown" }, hostOutput with { Text = new string('x', 65537) }, hostOutput with { Replace = true } })
    await ExpectAsync<InvalidDataException>(() => Read(hostStart + Frame(HostOutput(invalid))), "Invalid terminal output was accepted");
await ExpectAsync<InvalidDataException>(() => Read(hostStart + Frame(HostOutput(hostOutput)) + Frame(HostOutput(hostOutput))), "Duplicate terminal output was accepted");
await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(HostOutput(hostOutput))), "Unannounced terminal output was accepted");
await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(Event("tool_call") with { Tool = hostCall with { Name = "filesystem.read" } }) +
    Frame(HostOutput(hostOutput))), "Terminal output was associated with a nonterminal tool");
Check((await Read(hostStart + Frame(HostOutput(hostOutput with { Stream = "console", Replace = true })) + Frame(Event("interrupted", content: "partial"))))[2].Terminal?.Replace == true,
    "Explicit console-screen replacement was rejected");

var firstSegment = new AssistantSegment("fixture-round-1", 1, 0, "commentary", "streaming", "", "");
var firstCompleted = firstSegment with { Status = "completed", Content = "先核对来源。", Reasoning = "可公开的摘要", ReasoningDurationMs = 1000 };
var secondSegment = new AssistantSegment("fixture-round-2", 2, 2, "commentary", "streaming", "", "");
var finalSegment = secondSegment with { Phase = "final_answer", Status = "completed", Content = "这是核验后的最终答案。" };
ChatStreamEvent SegmentEvent(AssistantSegment segment) => Event("assistant_segment") with { Segment = segment, ToolStreamProtocol = 3 };
var finalSegmentEvent = Event("completed", content: finalSegment.Content) with { AssistantSegments = [firstCompleted, finalSegment], ToolStreamProtocol = 3 };
// Steering keeps the old call receipt but gives the next reply its own ordered segment sequence.
// 引导保留原调用回执，下一条回复使用独立的有序消息段，不混入旧回复正文。
Guid anchoredReplyId = Guid.NewGuid(), steeringUserMessageId = Guid.NewGuid();
var replyAnchor = Event("reply_anchor") with
{
    ReplyMessageId = anchoredReplyId, UserMessageId = steeringUserMessageId, SupersededReplyMessageId = requestId
};
var anchoredSegment = firstSegment with { Id = "steered-round-1" };
var anchoredFinal = anchoredSegment with { Phase = "final_answer", Status = "completed", Content = "明白，继续按调整后的要求交付。" };
var priorCall = new ToolActivity("before-steering", "filesystem.read", JsonSerializer.SerializeToElement(new { path = "guide.md" }),
    "running", "Read guide.md", Round: 1, Order: 1);
var anchoredEvents = await Read(Frame(Event("started") with { ReplyMessageId = requestId })
    + Frame(SegmentEvent(firstSegment)) + Frame(SegmentEvent(firstCompleted))
    + Frame(Event("tool_call") with { Tool = priorCall })
    + Frame(Event("tool_result") with { Tool = priorCall with { Status = "completed", Result = "保留的资料" } })
    + Frame(replyAnchor) + Frame(SegmentEvent(anchoredSegment)) + Frame(SegmentEvent(anchoredFinal))
    + Frame(Event("completed", content: anchoredFinal.Content) with { ReplyMessageId = anchoredReplyId,
        AssistantSegments = [anchoredFinal], ToolStreamProtocol = 3 }));
Check(anchoredEvents[0].ReplyMessageId == requestId && anchoredEvents[5].ReplyMessageId == anchoredReplyId
    && anchoredEvents[5].UserMessageId == steeringUserMessageId && anchoredEvents[5].SupersededReplyMessageId == requestId
    && anchoredEvents[^1].RequestId == requestId && anchoredEvents[^1].AssistantSegments!.SequenceEqual(new[] { anchoredFinal })
    && anchoredEvents.Single(item => item.Type == "tool_result").Tool!.ToolCallId == priorCall.ToolCallId,
    "Steering lost its reply anchor, earlier tool receipt, or independent final-answer sequence.");
var resumedAnchorEvents = await Read(Frame(Event("started") with { ReplyMessageId = anchoredReplyId })
    + Frame(replyAnchor with { SupersededReplyMessageId = null })
    + Frame(Event("content_snapshot", content: "接续已有资料", reasoning: "") with { ReplyMessageId = anchoredReplyId })
    + Frame(Event("completed", content: anchoredFinal.Content) with { ReplyMessageId = anchoredReplyId,
        AssistantSegments = [anchoredFinal], ToolStreamProtocol = 3 }));
Check(resumedAnchorEvents[2].Content == "接续已有资料" && resumedAnchorEvents[^1].Content == anchoredFinal.Content,
    "Resumed steering failed to route its snapshot and final reply to the current anchor.");
// The continuation marker can be finalized after the completed round; it does not create another stream event.
// 续写标记可在已完成轮次之后于终态确定；不重复发送已完成段，也不改变分段身份。
var continuedFirst = firstCompleted with { IsAnswerContinuation = true };
var continuedTerminal = finalSegmentEvent with { Content = firstCompleted.Content + "\n\n" + finalSegment.Content,
    AssistantSegments = [continuedFirst, finalSegment] };
var continuedEvents = await Read(prefix + Frame(SegmentEvent(firstSegment)) + Frame(SegmentEvent(firstCompleted)) +
    Frame(SegmentEvent(secondSegment)) + Frame(SegmentEvent(finalSegment)) + Frame(continuedTerminal));
Check(continuedEvents[^1].AssistantSegments![0].IsAnswerContinuation && !continuedEvents[^1].AssistantSegments![1].IsAnswerContinuation
    && continuedEvents[^1].Content == continuedTerminal.Content,
    "A terminal continuation marker preserves both answer chunks without emitting a duplicate completed round.");
Check(!continuedEvents[2].Segment!.IsAnswerContinuation,
    "Older unmarked public round events retain the default continuation value.");
var legacySegmentEvents = await Read(prefix + Frame(SegmentEvent(firstSegment))
    .Replace(",\"isAnswerContinuation\":false", "", StringComparison.Ordinal) + Frame(Event("interrupted", content: "partial")));
Check(!legacySegmentEvents[1].Segment!.IsAnswerContinuation,
    "Legacy SSE frames without a continuation field keep the default false value.");
await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(SegmentEvent(firstSegment with { IsAnswerContinuation = true }))),
    "A streaming segment was incorrectly accepted as a completed answer continuation");
await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(continuedTerminal with
    { AssistantSegments = [continuedFirst, finalSegment with { IsAnswerContinuation = true }] })),
    "The final phase was incorrectly accepted as another continuation");
// A repaired model step keeps its interrupted trace while the final answer must be complete.
// 已修复模型步骤保留中断轨迹，最终回答自身仍必须完整结束。
var repairedSegment = firstCompleted with { Status = "interrupted" };
var repairedTerminal = finalSegmentEvent with { AssistantSegments = [repairedSegment, finalSegment] };
Check((await Read(prefix + Frame(SegmentEvent(firstSegment)) + Frame(SegmentEvent(repairedSegment)) +
    Frame(SegmentEvent(secondSegment)) + Frame(SegmentEvent(finalSegment)) + Frame(repairedTerminal)))[^1].Type == "completed",
    "A safely recovered earlier step prevented the completed final answer");
await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(SegmentEvent(firstSegment)) + Frame(SegmentEvent(firstCompleted)) +
    Frame(SegmentEvent(secondSegment)) + Frame(SegmentEvent(finalSegment with { Status = "interrupted" })) +
    Frame(finalSegmentEvent with { AssistantSegments = [firstCompleted, finalSegment with { Status = "interrupted" }] })),
    "An interrupted final answer was accepted as completed");
string publicTimelineWire = "";
string largeWriteTimelineWire = "";
// The gateway can compact before generation, then stream text in the same still-open segment.
// 网关可以在生成前整理上下文，随后在同一个尚未完成的消息段中输出正文。
var contextActivity = new ToolActivity("compact-before-model", "context.compact", JsonSerializer.SerializeToElement(new { }),
    "running", "整理上下文", Round: 1, Order: 1);
var compactFinalSegment = firstSegment with { Phase = "final_answer", Status = "completed", Content = "你好！" };
var compactTerminal = Event("completed", content: compactFinalSegment.Content) with
{ AssistantSegments = [compactFinalSegment], ToolStreamProtocol = 3 };
string compactStart = prefix + Frame(SegmentEvent(firstSegment));
var compactEvents = await Read(compactStart + Frame(Event("tool_call") with { Tool = contextActivity }) +
    Frame(Event("tool_result") with { Tool = contextActivity with { Status = "completed", Result = "继续原任务。" } }) +
    Frame(Event("text_delta", "你好！") with { SegmentId = firstSegment.Id }) +
    Frame(SegmentEvent(compactFinalSegment)) + Frame(compactTerminal));
Check(compactEvents[^1].Content == "你好！" && compactEvents.Count(value => value.Type == "tool_result") == 1,
    "Preparation compaction closed the client stream before the model could answer");
await ExpectAsync<InvalidDataException>(() => Read(compactStart + Frame(Event("tool_call") with
{ Tool = contextActivity with { Arguments = JsonSerializer.SerializeToElement(new { path = "unexpected" }) } })),
    "A named context activity bypassed ordinary tool ordering with executable arguments");
await ExpectAsync<InvalidDataException>(() => Read(compactStart + Frame(Event("approval_required") with
{ Tool = contextActivity with { Status = "approval-required", ApprovalId = Guid.NewGuid() } })),
    "Display-only context maintenance requested execution approval");
await ExpectAsync<InvalidDataException>(() => Read(compactStart + Frame(Event("tool_call") with { Tool = contextActivity }) +
    Frame(compactTerminal)), "An unfinished context activity was accepted as a completed request");
using (var timelineArguments = JsonDocument.Parse("{\"url\":\"https://example.invalid\"}"))
{
    var timelineTool = new ToolActivity("timeline-call", "mcp.fixture.navigate", timelineArguments.RootElement.Clone(), "running", "Navigate", Round: 1, Order: 1);
    string timelineWire = prefix + Frame(SegmentEvent(firstSegment)) +
        Frame(Event("reasoning_delta", "可公开的摘要") with { SegmentId = firstSegment.Id }) +
        Frame(Event("text_delta", "先核对来源。") with { SegmentId = firstSegment.Id }) + Frame(SegmentEvent(firstCompleted)) +
        Frame(Event("tool_call") with { Tool = timelineTool }) +
        Frame(Event("tool_result") with { Tool = timelineTool with { Status = "completed", Result = "fixture source" } }) +
        Frame(SegmentEvent(secondSegment)) + Frame(Event("text_delta", finalSegment.Content) with { SegmentId = secondSegment.Id }) +
        Frame(SegmentEvent(finalSegment)) + Frame(finalSegmentEvent);
    publicTimelineWire = timelineWire;
    var timelineEvents = await Read(timelineWire);
    Check(timelineEvents[5].Tool?.Round == 1 && timelineEvents[7].Segment?.Round == 2 && timelineEvents[^1].AssistantSegments?.Length == 2,
        "Public round, tool and final-answer chronology was lost");
    Check(timelineEvents[^1].Content == finalSegment.Content && timelineEvents[^1].AssistantSegments![0].Content == firstCompleted.Content,
        "Final answer or retained commentary was lost");
    await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(SegmentEvent(firstSegment)) + Frame(Event("text_delta", "orphan") with { SegmentId = "unknown" })), "Unknown segment delta accepted");
    await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(SegmentEvent(firstSegment)) + Frame(Event("text_delta", "unbound"))), "Unbound timeline delta accepted");
    await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(SegmentEvent(firstSegment) with { ToolStreamProtocol = 4 })), "Future timeline protocol accepted");
    await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(SegmentEvent(firstSegment with { Phase = "private_thought" }))), "Unknown phase accepted");
    await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(SegmentEvent(firstSegment)) + Frame(SegmentEvent(firstCompleted with { Order = 5 }))), "Segment identity mutation accepted");
    await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(SegmentEvent(firstSegment)) + Frame(Event("tool_call") with { Tool = timelineTool })), "Tool executed before the public round completed");
    await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(SegmentEvent(firstSegment)) + Frame(SegmentEvent(firstCompleted)) + Frame(Event("completed", content: "missing segments"))), "Timeline completion silently discarded segments");
    await ExpectAsync<InvalidDataException>(() => Read(prefix + Frame(finalSegmentEvent with { AssistantSegments = [firstCompleted] })), "Commentary was accepted as final answer");
    Check((await Read(prefix + Frame(SegmentEvent(firstSegment)) + Frame(Event("interrupted", content: "partial"))))[^1].Type == "interrupted", "Interrupted open round rejected");
    var parallelCalls = Enumerable.Range(1, 4).Select(order => timelineTool with
    { ToolCallId = "parallel-" + order, Name = "filesystem.read", Arguments = JsonSerializer.SerializeToElement(new { path = "fixture-" + order + ".txt" }), Summary = "Read fixture source", Order = order }).ToArray();
    var parallelStart = prefix + Frame(SegmentEvent(firstSegment)) + Frame(SegmentEvent(firstCompleted)) +
        string.Concat(parallelCalls.Select(tool => Frame(Event("tool_call") with { Tool = tool })));
    var parallelNext = secondSegment with { Order = 5 };
    var parallelFinal = finalSegment with { Order = 5 };
    var parallelTerminal = finalSegmentEvent with { AssistantSegments = [firstCompleted, parallelFinal] };
    var completionOrder = new[] { 2, 0, 3, 1 };
    string parallelWire = parallelStart + string.Concat(completionOrder.Select(index =>
        Frame(Event("tool_result") with { Tool = parallelCalls[index] with { Status = "completed", Result = "fixture read " + index } }))) +
        Frame(SegmentEvent(parallelNext)) + Frame(SegmentEvent(parallelFinal)) + Frame(parallelTerminal);
    var parallelEvents = await Read(parallelWire);
    Check(parallelEvents.Where(value => value.Type == "tool_result").Select(value => value.Tool!.Order).SequenceEqual(new int?[] { 3, 1, 4, 2 }) && parallelEvents[^1].AssistantSegments![1].Order == 5,
        "Concurrent read receipts completing out of order corrupted round chronology");
    await ExpectAsync<InvalidDataException>(() => Read(parallelStart + Frame(Event("tool_result") with { Tool = parallelCalls[0] with { Status = "completed", Result = "changed position", Order = 2 } })), "Completed tool changed its stable order");
    await ExpectAsync<InvalidDataException>(() => Read(parallelStart + Frame(Event("approval_required") with { Tool = parallelCalls[0] with { Status = "approval-required", ApprovalId = Guid.NewGuid(), Order = 2 } })), "Approval changed its stable tool order");
    await ExpectAsync<InvalidDataException>(() => Read(parallelStart + Frame(SegmentEvent(parallelNext with { Order = 4 }))), "A new round reused a tool position");
}

// Exercise a real long-write frame without making every fixture byte a separate transport packet.
// 验证真实长文写入帧；已有测试覆盖逐字节分包，此处不把大夹具每个字节都拆成一个包。
const string longWriteBodyPrefix = "物理科普正文：";
string longWriteBody = longWriteBodyPrefix + new string('文', 120000);
var longWriteArguments = JsonSerializer.SerializeToElement(new { path = "physics.md", content = longWriteBody }, json);
var longWrite = new ToolActivity("long-document-write", "filesystem.write", longWriteArguments,
    "running", "写入物理长文", Round: 1, Order: 1);
var longApproval = longWrite with { Status = "approval-required", ApprovalId = Guid.NewGuid() };
largeWriteTimelineWire = prefix + Frame(SegmentEvent(firstSegment)) + Frame(SegmentEvent(firstCompleted))
    + Frame(Event("tool_call") with { Tool = longWrite })
    + Frame(Event("approval_required") with { Tool = longApproval })
    + Frame(Event("tool_result") with { Tool = longWrite with { Status = "completed", Result = "已写入并核验" } })
    + Frame(SegmentEvent(secondSegment)) + Frame(SegmentEvent(finalSegment)) + Frame(finalSegmentEvent);
using (var longWriteStream = new MemoryStream(Encoding.UTF8.GetBytes(largeWriteTimelineWire)))
{
    var longEvents = new List<ChatStreamEvent>();
    await foreach (var item in ChatStreamReader.ReadAsync(longWriteStream, conversationId, requestId)) longEvents.Add(item);
    Check(longEvents[^1].Type == "completed" && longEvents[^1].Content == finalSegment.Content,
        "A legitimate 120K-character document write interrupted its subsequent final answer.");
    Check(longEvents.Single(item => item.Type == "approval_required").Tool!.Arguments!.Value.GetProperty("content").GetString() == longWriteBody
        && longEvents.Single(item => item.Type == "tool_result").Tool!.Arguments!.Value.GetProperty("content").GetString() == longWriteBody,
        "Long-write approval and receipt parameters were truncated or changed.");
}
var oversizedWrite = longWrite with { Arguments = JsonSerializer.SerializeToElement(new
    { path = "too-large.md", content = new string('x', 1024 * 1024) }, json) };
await ExpectAsync<InvalidDataException>(async () =>
{
    using var stream = new MemoryStream(Encoding.UTF8.GetBytes(prefix + Frame(SegmentEvent(firstSegment))
        + Frame(SegmentEvent(firstCompleted)) + Frame(Event("tool_call") with { Tool = oversizedWrite })));
    await foreach (var _ in ChatStreamReader.ReadAsync(stream, conversationId, requestId)) { }
}, "Over-budget tool argument frames were accepted.");

var legacyText = JsonSerializer.Deserialize<ChatMessageState>("\"原有问题\"")!;
Check(legacyText.Content == "原有问题" && legacyText.Role == "user" && legacyText.Status == "completed", "Legacy plain text failed");
var legacyObject = JsonSerializer.Deserialize<ChatMessageState>("{\"Role\":\"assistant\",\"Content\":\"旧回复\"}")!;
Check(legacyObject.Status == "completed" && legacyObject.Reasoning == "" && legacyObject.ReasoningDurationMs == 0 && legacyObject.DurationMs == 0, "Legacy object defaults failed");
foreach (string invalidDuration in new[] { "-1", "1.5", "\"123\"", "9007199254740992" })
    await ExpectAsync<JsonException>(() => Task.Run(() => JsonSerializer.Deserialize<ChatMessageState>("{\"Role\":\"assistant\",\"Content\":\"test\",\"DurationMs\":" + invalidDuration + "}")), "Invalid stored duration accepted");
var saved = new ChatMessageState
{
    Id = requestId, Role = "assistant", Content = "最终回答", Reasoning = "公开的思考摘要",
    Status = "interrupted", Error = "已停止", Provider = "test-provider", Model = "test-model",
    ReasoningDurationMs = 1250, DurationMs = 7654, CreatedAt = createdAt
};
var restored = JsonSerializer.Deserialize<ChatMessageState>(JsonSerializer.Serialize(saved))!;
Check(restored.Id == saved.Id && restored.Content == saved.Content && restored.Reasoning == saved.Reasoning &&
    restored.Status == saved.Status && restored.Error == saved.Error && restored.Provider == saved.Provider &&
    restored.Model == saved.Model && restored.ReasoningDurationMs == saved.ReasoningDurationMs && restored.DurationMs == saved.DurationMs && restored.CreatedAt == saved.CreatedAt,
    "Streamed message persistence failed");
saved.SupersededByUserMessageId = steeringUserMessageId;
saved.InReplyToUserMessageId = steeringUserMessageId;
saved.OriginUserMessageId = userMessageId;
restored = JsonSerializer.Deserialize<ChatMessageState>(JsonSerializer.Serialize(saved))!;
Check(restored.SupersededByUserMessageId == steeringUserMessageId && restored.InReplyToUserMessageId == steeringUserMessageId
    && restored.OriginUserMessageId == userMessageId && legacyObject.SupersededByUserMessageId is null,
    "Steering visibility and reply relationships did not survive formal message JSON reopening.");
saved.Status = "streaming";
restored = JsonSerializer.Deserialize<ChatMessageState>(JsonSerializer.Serialize(saved))!;
Check(restored.Status == "interrupted" && restored.Content == saved.Content && restored.Reasoning == saved.Reasoning, "Crashed generation was not recovered");
saved.AssistantSegments = [firstCompleted, finalSegment]; saved.Status = "completed";
restored = JsonSerializer.Deserialize<ChatMessageState>(JsonSerializer.Serialize(saved))!;
Check(restored.AssistantSegments.SequenceEqual(saved.AssistantSegments), "Saved assistant rounds did not survive reopening");
saved.AssistantSegments = [continuedFirst, finalSegment];
restored = JsonSerializer.Deserialize<ChatMessageState>(JsonSerializer.Serialize(saved))!;
Check(restored.AssistantSegments[0].IsAnswerContinuation && restored.AssistantSegments.SequenceEqual(saved.AssistantSegments),
    "The continuation marker and original chunks survive persisted chat reopening.");
saved.Status = "streaming"; saved.AssistantSegments = [firstCompleted, secondSegment];
restored = JsonSerializer.Deserialize<ChatMessageState>(JsonSerializer.Serialize(saved))!;
Check(restored.AssistantSegments[0] == firstCompleted && restored.AssistantSegments[1].Status == "interrupted", "Crashed open round was not recovered independently");

// A local fake gateway verifies the real HTTP transport never waits for the whole response.
// 用本地伪网关验证真实 HTTP 传输，不应等待整个响应才交付流式内容。
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
            await context.Response.OutputStream.WriteAsync(JsonSerializer.SerializeToUtf8Bytes(new
            {
                status = "ok", service = "kynxa-model-gateway", runtime = "rust", conversationProtocol = 1,
                dataLayoutVersion = 1, memoryProtocol = 1, contextProtocol = 3, agentProtocol = 5,
                officialToolsProtocol = 2, hostTerminalProtocol = 3, browserAutomationProtocol = 2,
                extensionStorageProtocol = 1, toolStreamProtocol = 3, retrievalProtocol = 1,
                modelDataHome = ApplicationPaths.ModelHome, extensionRoot = ExtensionPaths.Root
            }));
            return;
        }
        if (context.Request.Url.AbsolutePath.StartsWith("/api/tasks/", StringComparison.Ordinal))
        {
            Check(context.Request.HttpMethod == "GET" && context.Request.QueryString["conversationId"] == conversationId.ToString("D")
                && context.Request.Headers["Accept"] == "text/event-stream", "Task observation changed scope or dispatched another request.");
            context.Response.ContentType = "text/event-stream";
            context.Response.SendChunked = true;
            await context.Response.OutputStream.WriteAsync(Encoding.UTF8.GetBytes(publicTimelineWire));
            return;
        }
        var incoming = await JsonSerializer.DeserializeAsync<ChatRequest>(context.Request.InputStream, json)
            ?? throw new InvalidDataException("Missing request body");
        Check(incoming.RequestId == requestId && incoming.ConversationId == conversationId, "Transport changed request IDs");
        Check(incoming.UserMessageId == userMessageId, "Transport omitted canonical user message identity");
        if (context.Request.Url.AbsolutePath == "/api/chat")
        {
            context.Response.ContentType = "application/json";
            long duration = incoming.Message == "bad-duration" ? -1 : 7654;
            await context.Response.OutputStream.WriteAsync(Encoding.UTF8.GetBytes(JsonSerializer.Serialize(
                new ChatReply(conversationId, requestId, "assistant", "Complete HTTP reply.", createdAt, duration), json)));
            return;
        }
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
        if (incoming.Message == "timeline")
        {
            await context.Response.OutputStream.WriteAsync(Encoding.UTF8.GetBytes(publicTimelineWire));
            return;
        }
        if (incoming.Message == "large-write")
        {
            await context.Response.OutputStream.WriteAsync(Encoding.UTF8.GetBytes(largeWriteTimelineWire));
            return;
        }
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
    var request = new ChatRequest(conversationId, "live", RequestId: requestId, UserMessageId: userMessageId);
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
    Check(received.Count == 3 && received[^1].Content == complete.Content && received[^1].DurationMs == 6789, "HTTP SSE reply or timing failed");
    Check((await client.ReplyAsync(request, testDeadline.Token)).DurationMs == 7654, "Nonstreaming timing was lost");
    await ExpectAsync<InvalidDataException>(() => client.ReplyAsync(request with { Message = "bad-duration" }, testDeadline.Token), "Invalid HTTP duration accepted");
    var publicRounds = new List<ChatStreamEvent>();
    await foreach (var item in client.StreamReplyAsync(request with { Message = "timeline" }, testDeadline.Token)) publicRounds.Add(item);
    Check(publicRounds.Count == 11 && publicRounds[^1].Content == finalSegment.Content && publicRounds[^1].AssistantSegments!.SequenceEqual(new[] { firstCompleted, finalSegment }),
        "Real HTTP transport lost the public timeline or final-answer boundary");
    var longWriteRounds = new List<ChatStreamEvent>();
    await foreach (var item in client.StreamReplyAsync(request with { Message = "large-write" }, testDeadline.Token)) longWriteRounds.Add(item);
    Check(longWriteRounds.Count == 9 && longWriteRounds[^1].Content == finalSegment.Content
        && longWriteRounds.Single(item => item.Type == "approval_required").Tool!.Arguments!.Value.GetProperty("content").GetString() == longWriteBody,
        "Real HTTP long-write transport lost complete approval parameters or the final answer.");
    var resumedRounds = new List<ChatStreamEvent>();
    await foreach (var item in client.StreamTaskReplyAsync(conversationId, Guid.NewGuid(), requestId, testDeadline.Token)) resumedRounds.Add(item);
    Check(resumedRounds.Count == 11 && resumedRounds[^1].Content == finalSegment.Content,
        "Resumed task observation returns the final answer without dispatching a new chat request.");
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
Console.WriteLine("PASS: fragmented Unicode/multiline SSE, v2 compatibility, v3 public-round/tool/final validation, cancellation, round persistence/crash recovery, real HTTP incremental and timeline delivery, error handling.");

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
