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

// Validate additive contracts through the existing client, using synthetic observations and no running gateway.
// 用现有客户端验证新增合同，仅使用合成观察，不访问正在运行的网关或真实用户数据。
var statusJson = """
    {
      "backend":"sqlite","sourceCount":20,"chunkCount":200,
      "embedding":{"state":"ready","profileId":"builtin-multilingual-dml-q8","dimensions":384,"available":true,
        "loaded":true,"inferenceBackend":{"device":"dml","deviceId":0,"cpuThreads":2,"dtype":"q8",
          "gpuValidated":true,"executionMode":"hybrid","cpuOperatorFallback":true},
        "resourceReservation":{"residentMemoryBytes":536870912}},
      "jobs":[{"jobId":"job_partial","status":"partial","completedSources":19,"totalSources":20,
        "coverage":{"discovered":20,"lexical":19,"semantic":15,"failed":1,"skipped":0,"partial":4,
          "complete":false,"reportTruncated":false},"semantic":{"state":"partial","priorDiagnosticCodes":[]}}],
      "resources":{"mode":"rust","gpu":{"state":"available","executionProvider":"dml"}},
      "vectorSpacePolicy":{"retainedSpaces":2,"migration":[{"state":"preparing"}]},
      "deployment":{"platform":"windows","otherPlatformsSupported":false},
      "externalModels":[{"backend":"ollama","loaded":true,"observationOnly":true}],
      "reranking":{"state":"disabled","loaded":false},"parsing":{"state":"ready"},
      "embeddingProfiles":[{"profileId":"builtin-multilingual","loaded":false}],
      "modelProfiles":[{"id":"builtin-multilingual-dml-q8"}],
      "configuredEmbeddingProfileId":"builtin-multilingual"
    }
    """;
transport.Respond = _ => new(HttpStatusCode.OK) { Content = new StringContent(statusJson, System.Text.Encoding.UTF8, "application/json") };
var observedStatus = await api.GetStatusAsync();
Check(observedStatus.Embedding.Loaded == true && observedStatus.Embedding.InferenceBackend?.Device == "dml",
    "Loaded backend survives typed transport; GPU availability alone is not substituted for execution.");
Check(observedStatus.Embedding.InferenceBackend?.ExecutionMode == "hybrid" && observedStatus.Embedding.InferenceBackend.CpuOperatorFallback == true,
    "Hybrid GPU execution retains its CPU operator disclosure.");
Check(observedStatus.Resources?.GetProperty("mode").GetString() == "rust" && observedStatus.VectorSpacePolicy?.GetProperty("retainedSpaces").GetInt32() == 2,
    "Resource and migration observations reach the client instead of being discarded.");
Check(observedStatus.Deployment?.GetProperty("platform").GetString() == "windows" && observedStatus.ExternalModels?.GetArrayLength() == 1,
    "Windows deployment scope and read-only external runtime observations are retained.");
Check(observedStatus.Jobs[0].Status == "partial" && observedStatus.Jobs[0].Coverage?.Failed == 1 && observedStatus.Jobs[0].Coverage?.Complete == false,
    "A partially indexed corpus keeps its gap counts and cannot be presented as completed coverage.");
Check(observedStatus.Jobs[0].Semantic?.GetProperty("state").GetString() == "partial" && observedStatus.Reranking?.Loaded == false,
    "Partial semantic indexing and an unloaded reranker remain distinct.");
Check(observedStatus.EmbeddingProfiles?.GetArrayLength() == 1 && observedStatus.ModelProfiles?.GetArrayLength() == 1 && observedStatus.Parsing?.GetProperty("state").GetString() == "ready",
    "Per-profile observations, supported profiles and parser state survive the contract.");
Check(observedStatus.ConfiguredEmbeddingProfileId == "builtin-multilingual" && observedStatus.Embedding.InferenceBackend?.Device == "dml",
    "Configured profile stays separate from the observed execution backend. / 配置项与实际执行后端保持分离。");

var extendedLocal = JsonSerializer.Deserialize<RetrievalLocalSettings>("""
    {"enabled":true,"semantic":"auto","embeddingProfileId":"builtin-multilingual","embeddingDevicePolicy":"gpu",
     "indexing":{"maximumFiles":50000,"maximumSourceBytes":33554432,"maximumTotalBytes":8589934592,"maximumEntries":300000,"batchSize":64},
     "ann":{"adaptive":true,"mode":"auto","threshold":10000,"maxCachedShards":8,"maxShardBytes":1073741824,
       "connectivity":32,"expansionAdd":256,"expansionSearch":128,"exactScanLimit":1000000}}
    """, jsonOptions)!;
Check(extendedLocal.EmbeddingDevicePolicy == "gpu" && extendedLocal.Indexing?.MaximumTotalBytes == 8589934592,
    "Device policy and indexing capacity preserve values beyond 32-bit byte counts.");
Check(extendedLocal.Ann?.Adaptive == true && extendedLocal.Ann?.ExpansionAdd == 256,
    "ANN policy retains independent adaptive and graph settings.");
transport.Respond = _ => Reply(global);
await api.SaveSettingsAsync(new(7, new(Local: global.Local with { Semantic = "off" })));
using (var patch = JsonDocument.Parse(transport.LastBody!))
{
    var local = patch.RootElement.GetProperty("patch").GetProperty("local");
    Check(!local.TryGetProperty("embeddingDevicePolicy", out _) && !local.TryGetProperty("indexing", out _) && !local.TryGetProperty("ann", out _),
        "Legacy UI updates omit unset new fields, preserving backend-owned settings.");
}
var indexingPatch = JsonSerializer.Serialize(new RetrievalIndexingLimits(BatchSize: 16), jsonOptions);
Check(indexingPatch == "{\"batchSize\":16}", "Partial indexing patches do not inject unrelated default limits.");
var annPatch = JsonSerializer.Serialize(new RetrievalAnnSettings(Adaptive: false), jsonOptions);
Check(annPatch == "{\"adaptive\":false}", "Explicit false survives an ANN patch while unspecified knobs remain absent.");
var legacyStatus = JsonSerializer.Deserialize<RetrievalStatus>("""
    {"backend":"sqlite","sourceCount":0,"chunkCount":0,
     "embedding":{"state":"ready","profileId":"builtin-multilingual","dimensions":384,"available":true},"jobs":[]}
    """, jsonOptions)!;
Check(legacyStatus.Resources is null && legacyStatus.Embedding.Loaded is null && legacyStatus.VectorSpacePolicy is null,
    "Old status JSON stays compatible and unknown observations remain null.");
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
