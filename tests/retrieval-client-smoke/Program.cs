using System.Net;
using System.Net.Http.Json;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;

if (args.Contains("--live", StringComparer.Ordinal))
{
    await LiveGatewayChecks.RunAsync();
    return;
}

var jsonOptions = new JsonSerializerOptions(JsonSerializerDefaults.Web);
int checks = 0;
void Check(bool condition, string message)
{
    if (!condition) throw new InvalidOperationException(message);
    checks++;
}
var projectId = Guid.NewGuid();
var global = new RetrievalSettingsDocument(1, 7, new(true, "auto", "builtin-multilingual"),
    new("auto", "auto", "standard", "auto"), new(67108864, 536870912));
var project = new ProjectRetrievalSettingsDocument(1, projectId, 3, new(), new(new(false, 2), []),
    new(global.Local, global.Web, global.Cache, new(false, 2, [])));
var transport = new RecordingHandler();
using var http = new HttpClient(transport) { BaseAddress = new Uri("http://127.0.0.1:1") };
int readyCalls = 0;
using var api = new RetrievalApiClient(http, token => { token.ThrowIfCancellationRequested(); readyCalls++; return Task.CompletedTask; });
HttpResponseMessage Reply(object response) => new(HttpStatusCode.OK) { Content = JsonContent.Create(response, options: jsonOptions) };
transport.Respond = _ => Reply(global);
Check((await api.GetSettingsAsync()).Revision == 7, "Global settings revision survives transport.");
Check(transport.LastMethod == HttpMethod.Get && transport.LastPath == "/api/retrieval/settings", "Global load uses the formal retrieval endpoint.");
await api.SaveSettingsAsync(new(7, new(Web: global.Web with { Depth = "deep" })));
using (var patch = JsonDocument.Parse(transport.LastBody!))
{
    Check(patch.RootElement.GetProperty("expectedRevision").GetInt64() == 7, "Global writes carry the expected revision.");
    Check(!patch.RootElement.GetProperty("patch").TryGetProperty("local", out _), "Web-only updates do not overwrite local settings.");
}
transport.Respond = _ => Reply(project);
Check((await api.GetProjectSettingsAsync(projectId)).ProjectId == projectId, "Project identity is checked against its target.");
await api.SaveProjectSettingsAsync(projectId, new(3, new(new(), new(new(true)))));
using (var patch = JsonDocument.Parse(transport.LastBody!))
{
    var overrides = patch.RootElement.GetProperty("patch").GetProperty("overrides");
    Check(overrides.GetProperty("local").ValueKind == JsonValueKind.Null && overrides.GetProperty("web").ValueKind == JsonValueKind.Null,
        "Clearing overrides explicitly resets inheritance.");
    var mounted = patch.RootElement.GetProperty("patch").GetProperty("indexingSources").GetProperty("mountedFolder");
    Check(mounted.GetProperty("enabled").GetBoolean() && !mounted.TryGetProperty("bindingRevision", out _),
        "Mounted-folder updates leave binding revisions under gateway ownership.");
}
transport.Respond = _ => Reply(project with { ProjectId = Guid.NewGuid() });
try { await api.GetProjectSettingsAsync(projectId); throw new Exception("Wrong-project response was accepted."); }
catch (InvalidDataException) { checks++; }
transport.Respond = _ => new(HttpStatusCode.Conflict) { Content = JsonContent.Create(new { code = "RETRIEVAL_REVISION_CONFLICT", error = "Synthetic conflict" }) };
try { await api.SaveSettingsAsync(new(7, new(Local: global.Local))); throw new Exception("A stale write was accepted."); }
catch (GatewayApiException error)
{
    Check(error.StatusCode == HttpStatusCode.Conflict && error.ErrorCode == "RETRIEVAL_REVISION_CONFLICT", "Conflict HTTP status and machine code remain available to the UI.");
}
transport.Respond = _ => Reply(new RetrievalSourcesResponse([]));
await api.GetSourcesAsync(projectId);
Check(transport.LastPath == $"/api/retrieval/sources?projectId={projectId:D}", "Source lists carry the selected project identity.");
var source = new RetrievalSource("source_test", "Synthetic source", "project", projectId, "synthetic-source.txt", "ready");
transport.Respond = _ => Reply(source);
await api.ImportSourceAsync(new("project", source.Path, projectId));
Check(transport.LastMethod == HttpMethod.Post, "Import is a gateway command.");
await api.DeleteSourceAsync(source.Id, 9);
using (var deletion = JsonDocument.Parse(transport.LastBody!))
    Check(deletion.RootElement.GetProperty("expectedRevision").GetInt64() == 9, "Source removal can carry a source revision.");
var job = new RetrievalIndexJob("job_test", "queued", 0, 2);
transport.Respond = _ => Reply(job);
await api.RebuildIndexAsync(projectId);
Check(transport.LastPath == "/api/retrieval/index/rebuild", "Rebuild uses an asynchronous job endpoint.");
await api.GetIndexJobAsync(job.JobId);
await api.CancelIndexJobAsync(job.JobId);
Check(transport.LastMethod == HttpMethod.Post && transport.LastPath?.EndsWith("/job_test/cancel") == true, "Cancellation targets the owned job.");
int beforeInvalidPath = transport.Requests;
try { await api.GetIndexJobAsync("../../private"); throw new Exception("Unsafe job ID was accepted."); }
catch (ArgumentException) { checks++; }
Check(transport.Requests == beforeInvalidPath, "Unsafe path components never reach HTTP.");
using var cancellation = new CancellationTokenSource();
cancellation.Cancel();
try { await api.GetSettingsAsync(cancellation.Token); throw new Exception("Cancelled load was accepted."); }
catch (OperationCanceledException) { checks++; }
Check(readyCalls >= 12, "All requests initialize or validate gateway availability.");
Console.WriteLine($"Retrieval client smoke passed: {checks} checks; no real gateway, credentials, or source data used.");

sealed class RecordingHandler : HttpMessageHandler
{
    public Func<HttpRequestMessage, HttpResponseMessage> Respond { get; set; } = _ => new(HttpStatusCode.NoContent);
    public HttpMethod? LastMethod { get; private set; }
    public string? LastPath { get; private set; }
    public string? LastBody { get; private set; }
    public int Requests { get; private set; }

    protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
    {
        Requests++;
        LastMethod = request.Method;
        LastPath = request.RequestUri!.PathAndQuery;
        LastBody = request.Content is null ? null : await request.Content.ReadAsStringAsync(cancellationToken);
        return Respond(request);
    }
}
