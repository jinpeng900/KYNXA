using System.Net;
using System.Net.Http.Json;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;

var conversation = Guid.NewGuid();
var project = Guid.NewGuid();
var taskId = Guid.NewGuid();
var nextId = Guid.NewGuid();
var task = new TaskRecord(taskId, Guid.NewGuid().ToString("D"), "索引双语资料", [], new(conversation, project), null, [],
    new(256, null), new(2, 1, 1, 1, 10, 20, 1), "paused", 3, 1, null, 8, "user-paused", 100, 200);
var transport = new Handler();
using var http = new HttpClient(transport) { BaseAddress = new("http://tasks.test.invalid") };
using var api = new TasksApiClient(http);
int checks = 0;
void Check(bool value, string message) { if (!value) throw new InvalidOperationException(message); checks++; }
HttpResponseMessage Reply(object body) => new(HttpStatusCode.OK) { Content = JsonContent.Create(body) };
async Task Reject(Func<Task> action)
{
    try { await action(); throw new Exception("Invalid task projection was accepted."); }
    catch (InvalidDataException) { checks++; }
}

transport.Reply = _ => Reply(new TaskListResponse([task], true, nextId));
var page = await api.ListAsync(conversation, project);
Check(page.Tasks[0].Usage.UnknownTokenRequests == 1, "Unknown token usage remains unknown, not inferred.");
Check(transport.Path == $"/api/tasks?conversationId={conversation:D}&projectId={project:D}", "List is bound to the current saved scope.");
await api.ListAsync(conversation, project, taskId);
Check(transport.Path!.EndsWith($"&afterTaskId={taskId:D}"), "Pagination uses the durable task cursor.");
var sameWorkChat = task with { TaskId = Guid.NewGuid(), Scope = new(Guid.NewGuid(), project) };
transport.Reply = _ => Reply(new TaskListResponse([task, sameWorkChat], false, null));
Check((await api.ListAsync(conversation, project)).Tasks[1].Scope.ConversationId == sameWorkChat.Scope.ConversationId,
    "The project list accepts other authorized chats in the same saved project.");
transport.Reply = _ => Reply(new TaskListResponse([task with { Scope = new(Guid.NewGuid(), null) }], false, null));
await Reject(() => api.ListAsync(conversation));
transport.Reply = _ => Reply(new TaskListResponse([task with { Scope = new(conversation, Guid.NewGuid()) }], false, null));
await Reject(() => api.ListAsync(conversation, project));
transport.Reply = _ => Reply(new TaskListResponse([task], true, taskId));
await Reject(() => api.ListAsync(conversation, project, taskId));

// A fresh page has no process-local pause map; only the bounded formal lookup can restore its identity.
// 新页面没有进程内暂停映射；只能从有界正式查询恢复身份，不能猜选或另建任务。
transport.Reply = _ => Reply(new TaskPausedResponse([]));
var nonePaused = PausedTaskSelection.From(await api.PausedAsync(conversation, project));
Check(nonePaused.TaskId is null && !nonePaused.IsAmbiguous, "No paused checkpoint permits a new ordinary request.");
Check(transport.Path == $"/api/tasks/paused?conversationId={conversation:D}&projectId={project:D}",
    "Pause recovery is scoped to one saved chat rather than the whole project's task pages.");
transport.Reply = _ => Reply(new TaskPausedResponse([task]));
var restored = PausedTaskSelection.From(await api.PausedAsync(conversation, project));
Check(restored.TaskId == taskId && !restored.IsAmbiguous,
    "A fresh client restores the unique durable paused task without any previous in-memory identity.");
transport.Reply = _ => Reply(new TaskPausedResponse([task, task with { TaskId = nextId }]));
var ambiguous = PausedTaskSelection.From(await api.PausedAsync(conversation, project));
Check(ambiguous.IsAmbiguous && ambiguous.TaskId is null,
    "Multiple paused roots require explicit task selection and cannot silently start a new task.");
Check(PausedTaskSelection.CanResumeInContext(task, conversation, project, false, null, false) &&
    PausedTaskSelection.CanResumeInContext(task, conversation, project, true, taskId, true),
    "An explicit selection can resume in its original chat with no reply or its confirmed paused observer.");
Check(!PausedTaskSelection.CanResumeInContext(task, conversation, project, true, nextId, false) &&
    !PausedTaskSelection.CanResumeInContext(task, conversation, project, true, null, false) &&
    !PausedTaskSelection.CanResumeInContext(task, conversation, project, true, taskId, false),
    "A new active reply, unregistered pending reply or old stream still closing blocks a stale resume decision.");
Check(!PausedTaskSelection.CanResumeInContext(task, Guid.NewGuid(), project, false, null, false) &&
    !PausedTaskSelection.CanResumeInContext(task, conversation, Guid.NewGuid(), false, null, false),
    "Navigation and project moves invalidate a paused selection before it can control the old task.");
transport.Reply = _ => Reply(new TaskPausedResponse([sameWorkChat]));
await Reject(() => api.PausedAsync(conversation, project));
transport.Reply = _ => Reply(new TaskPausedResponse([task with { Scope = new(conversation, Guid.NewGuid()) }]));
await Reject(() => api.PausedAsync(conversation, project));
transport.Reply = _ => Reply(new TaskPausedResponse([task with { State = "running" }]));
await Reject(() => api.PausedAsync(conversation, project));
transport.Reply = _ => Reply(new TaskPausedResponse([task with { ParentTaskId = nextId }]));
await Reject(() => api.PausedAsync(conversation, project));
transport.Reply = _ => Reply(new TaskPausedResponse([task, task]));
await Reject(() => api.PausedAsync(conversation, project));
transport.Reply = _ => Reply(new TaskPausedResponse([task, task with { TaskId = nextId }, task with { TaskId = Guid.NewGuid() }]));
await Reject(() => api.PausedAsync(conversation, project));
int beforeRecoveryFailure = transport.Requests;
transport.Reply = _ => new(HttpStatusCode.ServiceUnavailable)
    { Content = JsonContent.Create(new { error = "Task store unavailable.", code = "TASK_STORE_UNAVAILABLE" }) };
try
{
    await api.PausedAsync(conversation, project);
    throw new Exception("A failed checkpoint read was treated as no paused work.");
}
catch (GatewayApiException) { checks++; }
Check(transport.Requests == beforeRecoveryFailure + 2 && transport.Path!.StartsWith("/api/tasks/paused?"),
    "A transient recovery failure retries only the read once; it never dispatches a chat request.");

var hiddenRecovery = task with { DisplayInTaskList = false, Title = "恢复资料整理" };
var waitingRecovery = task with { TaskId = nextId, State = "waiting-verification", Title = "核实上次写入" };
transport.Reply = _ => Reply(new TaskListResponse([hiddenRecovery, waitingRecovery], true, nextId));
var recoverablePage = await api.RecoverableAsync(conversation, project);
Check(recoverablePage.Tasks.Length == 2 && recoverablePage.Tasks[0].DisplayInTaskList == false &&
    recoverablePage.Tasks[1].State == "waiting-verification" && recoverablePage.Tasks[1].Title == "核实上次写入",
    "Recovery lists retain hidden paused roots and verification-waiting roots with their real titles and states.");
Check(transport.Path == $"/api/tasks/recoverable?conversationId={conversation:D}&limit=50&projectId={project:D}",
    "Recovery choices use the same-chat endpoint rather than the project's ordinary task list.");
transport.Reply = _ => Reply(new TaskListResponse([hiddenRecovery], false, null));
await api.RecoverableAsync(conversation, project, nextId);
Check(transport.Path!.EndsWith($"&afterTaskId={nextId:D}"), "Recovery choices can load every page without truncating to the ambiguous shortcut's two roots.");
foreach (var invalidRoot in new[] { sameWorkChat, task with { State = "completed" }, task with { ParentTaskId = nextId } })
{
    transport.Reply = _ => Reply(new TaskListResponse([invalidRoot], false, null));
    await Reject(() => api.RecoverableAsync(conversation, project));
}

var child = task with { TaskId = Guid.NewGuid(), RequestId = "child:stable-request-deduplication-key", ParentTaskId = taskId, Goal = "读取文档" };
transport.Reply = _ => Reply(new TaskDetailResponse(task, [child], [], null, []));
Check((await api.GetAsync(conversation, taskId, project)).Children[0].RequestId == child.RequestId,
    "Namespaced child request keys remain strings while task identity remains a UUID.");
transport.Reply = _ => Reply(new TaskDetailResponse(task, [child with { ParentTaskId = Guid.NewGuid() }], [], null, []));
await Reject(() => api.GetAsync(conversation, taskId, project));

transport.Reply = _ => Reply(new TaskControlResponse(task with { State = "queued", Version = 4 }, [taskId, child.TaskId]));
var controlled = await api.ControlAsync(conversation, taskId, 3, "resume", project);
using (var body = JsonDocument.Parse(transport.Body!))
    Check(body.RootElement.GetProperty("expectedVersion").GetUInt64() == 3
        && body.RootElement.GetProperty("conversationId").GetGuid() == conversation
        && body.RootElement.GetProperty("action").GetString() == "resume", "Explicit controls carry the bound chat and observed version.");
Check(controlled.Task.Version == 4, "Only the gateway changes authoritative task versions.");
var waitingEvidence = JsonSerializer.SerializeToElement(new
{
    resolvedOperations = 1, resolvedReservations = 0,
    pending = new[] { new { code = "MODEL_REQUEST_UNKNOWN", message = "上次请求结果仍待核实。" } }
});
transport.Reply = _ => Reply(new TaskControlResponse(waitingRecovery, [nextId], Recovery: waitingEvidence));
var notYetResumed = await api.ControlAsync(conversation, nextId, waitingRecovery.Version, "resume", project);
Check(notYetResumed.Task.TaskId == nextId && notYetResumed.Task.State == "waiting-verification" &&
    notYetResumed.Recovery!.Value.GetProperty("pending")[0].GetProperty("message").GetString() == "上次请求结果仍待核实。",
    "An explicit root selection retains its identity and unresolved recovery evidence without reporting a successful resume.");
var steeringMessage = Guid.NewGuid();
transport.Reply = _ => Reply(new TaskControlResponse(task with { State = "queued", Version = 5 }, [taskId], true));
var steered = await api.SteerAsync(conversation, taskId, 4, steeringMessage, "保留已有结果，只补缺少的实验", "update", project);
using (var body = JsonDocument.Parse(transport.Body!))
    Check(transport.Path == $"/api/tasks/{taskId:D}/steer" && body.RootElement.GetProperty("userMessageId").GetGuid() == steeringMessage
        && body.RootElement.GetProperty("expectedVersion").GetUInt64() == 4 && body.RootElement.GetProperty("action").GetString() == "update",
        "Steering submits a real stable user message identity, explicit action and observed task version.");
Check(steered.ReconnectRequired && steered.Task.TaskId == taskId, "Steering retains the task identity and explicitly reconnects its observation.");
string allowedChineseUpdate = new string('文', 10922) + "ab";
Check(System.Text.Encoding.UTF8.GetByteCount(allowedChineseUpdate) == 32768, "The Chinese steering boundary uses UTF-8 bytes.");
int beforeChineseBoundary = transport.Requests;
await api.SteerAsync(conversation, taskId, 4, Guid.NewGuid(), allowedChineseUpdate, "continue", project);
Check(transport.Requests == beforeChineseBoundary + 1, "A Chinese steering message at 32768 UTF-8 bytes reaches the gateway.");
int beforeOversizedChineseUpdate = transport.Requests;
try
{
    await api.SteerAsync(conversation, taskId, 4, Guid.NewGuid(), allowedChineseUpdate + "文", "update", project);
    throw new Exception("Oversized Chinese task steering was accepted.");
}
catch (ArgumentException) { checks++; }
Check(transport.Requests == beforeOversizedChineseUpdate, "An oversized Chinese update is rejected before any message/control is dispatched.");
int beforeConflict = transport.Requests;
transport.Reply = _ => new(HttpStatusCode.Conflict) { Content = JsonContent.Create(new { error = "任务已变化", code = "TASK_VERSION_CONFLICT" }) };
try { await api.ControlAsync(conversation, taskId, 3, "cancel", project); throw new Exception("Expected conflict."); }
catch (GatewayApiException error) { Check(error.StatusCode == HttpStatusCode.Conflict && error.ErrorCode == "TASK_VERSION_CONFLICT", "Conflict identity survives transport."); }
Check(transport.Requests == beforeConflict + 1, "A control conflict never causes automatic replay.");
int readAttempts = 0;
transport.Reply = _ => ++readAttempts == 1
    ? new(HttpStatusCode.ServiceUnavailable) { Content = JsonContent.Create(new { error = "暂时不可用", code = "TEMPORARY_UNAVAILABLE" }) }
    : Reply(new TaskListResponse([task], false, null));
Check((await api.ListAsync(conversation, project)).Tasks.Length == 1 && readAttempts == 2,
    "A temporary projection read can recover once without replaying any side effect.");

var ev = new TaskEvent(taskId, 9, "receipt-9", "tool.result", JsonSerializer.SerializeToElement(new { state = "unknown" }), 200);
transport.Reply = _ => Reply(new TaskEventsResponse(taskId, [ev], 9, false));
Check((await api.EventsAsync(conversation, taskId, 8)).ThroughSequence == 9, "Events resume after the confirmed sequence.");
transport.Reply = _ => Reply(new TaskEventsResponse(taskId, [ev, ev], 9, false));
await Reject(() => api.EventsAsync(conversation, taskId, 8));
transport.Reply = _ => Reply(new TaskEventsResponse(taskId, [ev with { TaskId = Guid.NewGuid() }], 9, false));
await Reject(() => api.EventsAsync(conversation, taskId, 8));
Check(TaskStates.CanResume("waiting-verification") && !TaskStates.CanResume("completed"), "Unknown side effects wait for verification; ended tasks cannot be resumed.");
Console.WriteLine($"Task client smoke: {checks} checks passed.");

sealed class Handler : HttpMessageHandler
{
    public Func<HttpRequestMessage, HttpResponseMessage> Reply { get; set; } = _ => throw new InvalidOperationException("No fixture response.");
    public string? Path { get; private set; }
    public string? Body { get; private set; }
    public int Requests { get; private set; }
    protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested(); Requests++; Path = request.RequestUri!.PathAndQuery;
        Body = request.Content is null ? null : await request.Content.ReadAsStringAsync(cancellationToken);
        return Reply(request);
    }
}
