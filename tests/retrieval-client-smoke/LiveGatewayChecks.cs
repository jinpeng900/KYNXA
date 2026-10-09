using System.Diagnostics;
using System.Net;
using System.Net.Http.Json;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;

internal static class LiveGatewayChecks
{
    public static async Task RunAsync()
    {
        var repository = FindRepositoryRoot();
        var start = new ProcessStartInfo("node") { WorkingDirectory = repository, UseShellExecute = false, CreateNoWindow = true,
            RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true };
        start.ArgumentList.Add(Path.Combine(repository, "tests", "retrieval-client-smoke", "live-gateway-fixture.mjs"));
        using var fixture = Process.Start(start) ?? throw new InvalidOperationException("Could not start the owned gateway fixture.");
        var errorsTask = fixture.StandardError.ReadToEndAsync();
        int checks = 0;
        void Check(bool condition, string description)
        {
            if (!condition) throw new InvalidOperationException(description);
            checks++;
        }
        void CheckLexicalFallback(RetrievalIndexJob job, int expectedSources, string scope)
        {
            // This fixture deliberately lacks assets: lexical publication succeeds, but semantic coverage cannot.
            // 本夹具故意缺少模型资产：词法发布成功，语义覆盖仍不可用，不能把 partial 当成全量成功。
            Check(job.Status == "partial" && job.FinishedAt is not null && job.CompletedSources == expectedSources &&
                job.TotalSources == expectedSources, $"Real {scope} import finishes as partial with every eligible source processed.");
            var coverage = job.Coverage ?? throw new InvalidDataException($"Real {scope} import omitted its coverage receipt.");
            Check(coverage.Discovered == expectedSources && coverage.Lexical == expectedSources && coverage.Semantic == 0 &&
                coverage.Partial == expectedSources && coverage.Failed == 0 && coverage.Skipped == 0 &&
                !coverage.Complete && coverage.ReportTruncated == false,
                $"Real {scope} coverage confirms lexical publication, no semantic coverage and an incomplete corpus without read failures.");
            var semantic = job.Semantic ?? throw new InvalidDataException($"Real {scope} import omitted semantic diagnostics.");
            Check(semantic.GetProperty("requested").GetBoolean() && semantic.GetProperty("state").GetString() == "unavailable" &&
                semantic.GetProperty("vectorChunks").GetInt32() == 0 && semantic.GetProperty("totalChunks").GetInt32() >= expectedSources &&
                semantic.GetProperty("diagnosticCodes").EnumerateArray().Any(code => code.GetString() == "EMBEDDING_ASSET_MISSING"),
                $"Real {scope} semantic diagnostics identify the intentionally missing embedding assets without inventing vectors.");
        }
        try
        {
            string line = await fixture.StandardOutput.ReadLineAsync().WaitAsync(TimeSpan.FromSeconds(25))
                ?? throw new InvalidOperationException("Gateway fixture exited before readiness.");
            var info = JsonSerializer.Deserialize<FixtureInfo>(line, new JsonSerializerOptions(JsonSerializerDefaults.Web))
                ?? throw new InvalidDataException("Gateway fixture readiness is invalid.");
            using var http = new HttpClient { BaseAddress = new Uri(info.Address), Timeout = TimeSpan.FromSeconds(25) };
            using var api = new RetrievalApiClient(http);
            using var initialCatalog = await http.GetFromJsonAsync<JsonDocument>("/api/conversations/catalog");
            var global = await api.GetSettingsAsync();
            Check(global.SchemaVersion == 1 && global.Local.Enabled && global.Local.EmbeddingProfileId == "builtin-multilingual",
                "Real global settings deserialize with the default multilingual profile.");
            global = await api.SaveSettingsAsync(new(global.Revision, new(Web: global.Web with { Depth = "deep" })));
            Check(global.Web.Depth == "deep" && global.Revision == 1, "Real CAS update changes web settings immediately.");
            try { await api.SaveSettingsAsync(new(0, new(Web: global.Web))); throw new Exception("Stale CAS was accepted."); }
            catch (GatewayApiException error) { Check(error.StatusCode == HttpStatusCode.Conflict, "Real stale CAS preserves HTTP 409."); }

            var project = await api.GetProjectSettingsAsync(info.ProjectId);
            Check(project.ProjectId == info.ProjectId && project.Overrides.Web is null && project.Effective.Web.Depth == "deep",
                "Real project response includes effective inherited settings.");
            project = await api.SaveProjectSettingsAsync(info.ProjectId,
                new(project.Revision, new(new(Web: global.Web with { Depth = "standard" }), new(new(false)))));
            Check(project.Effective.Web.Depth == "standard" && (await api.GetSettingsAsync()).Web.Depth == "deep",
                "Real project web override leaves global settings unchanged.");
            project = await api.SaveProjectSettingsAsync(info.ProjectId, new(project.Revision, new(new(), new(new(false)))));
            Check(project.Overrides.Web is null && project.Effective.Web.Depth == "deep", "Real null overrides restore inheritance.");
            Check((await api.GetProvidersAsync()).Providers.Length == 0, "Provider listing does not start external MCP services.");

            var importedGlobal = await api.ImportSourceAsync(new("user", info.GlobalSource));
            Check(importedGlobal.Scope == "user" && importedGlobal.ProjectId is null && importedGlobal.Revision == 1 &&
                importedGlobal.ImportedCount == 1 && importedGlobal.JobId is not null, "Real source import exposes its source revision and owned job.");
            var globalJob = await WaitForJobAsync(api, importedGlobal.JobId!);
            CheckLexicalFallback(globalJob, 1, "global");
            var importedProject = await api.ImportSourceAsync(new("project", info.ProjectSource, info.ProjectId));
            Check(importedProject.ProjectId == info.ProjectId && importedProject.Scope == "project", "Real project source retains its stable owner.");
            var projectJob = await WaitForJobAsync(api, importedProject.JobId!);
            CheckLexicalFallback(projectJob, 2, "project and inherited global");
            var globalSources = await api.GetSourcesAsync();
            var visibleSources = await api.GetSourcesAsync(info.ProjectId);
            Check(globalSources.Sources.Length == 1 && globalSources.Sources.All(source => source.Scope == "user"), "Global source lists exclude project sources.");
            Check(visibleSources.Sources.Length == 2 && visibleSources.Sources.Any(source => source.Scope == "user") &&
                visibleSources.Sources.Any(source => source.ProjectId == info.ProjectId), "Project retrieval source lists include authorized inherited global sources.");

            var status = await api.GetStatusAsync();
            Check(status.Backend == "sqlite" && status.SourceCount == 2 && status.ChunkCount >= 2,
                "Real index status uses the public source and chunk count fields.");
            Check(status.Embedding.State == "unavailable" && !status.Embedding.Available && status.Embedding.Dimensions == 384,
                "Missing model assets report degraded lexical mode rather than pretending semantic readiness.");
            await api.DeleteSourceAsync(importedProject.Id, importedProject.Revision);
            Check(!(await api.GetSourcesAsync(info.ProjectId)).Sources.Any(source => source.Id == importedProject.Id), "Real removal revokes the project source.");
            await api.DeleteSourceAsync(importedGlobal.Id);
            Check((await api.GetSourcesAsync()).Sources.Length == 0, "Omitted optional delete revision does not become a null conflict.");
            var rebuild = await api.RebuildIndexAsync(info.ProjectId);
            Check(rebuild.Status is "queued" or "running" or "paused" or "completed" or "partial", "Manual rebuilding returns a valid durable job.");
            var cancellation = await api.CancelIndexJobAsync(rebuild.JobId);
            Check(cancellation.JobId == rebuild.JobId, "Cancellation acknowledges the selected durable job.");
            var rebuilt = await WaitForJobAsync(api, rebuild.JobId, allowCancelled: true);
            Check(rebuilt.JobId == rebuild.JobId && (rebuilt.Status is "completed" or "cancelled"),
                "The empty-source manual job either completes or acknowledges its requested cancellation without a coverage gap.");
            using var finalCatalog = await http.GetFromJsonAsync<JsonDocument>("/api/conversations/catalog");
            Check(initialCatalog!.RootElement.GetRawText() == finalCatalog!.RootElement.GetRawText(),
                "Settings, import, indexing and removal never create or modify a conversation.");
            Console.WriteLine($"Retrieval real HTTP integration passed: {checks} checks; SQLite jobs and source snapshots exercised in owned temporary data.");
        }
        catch (Exception error)
        {
            Console.Error.WriteLine(error);
            throw;
        }
        finally
        {
            if (!fixture.HasExited)
            {
                await fixture.StandardInput.WriteLineAsync("shutdown");
                fixture.StandardInput.Close();
                try { await fixture.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(15)); }
                catch (TimeoutException) { fixture.Kill(entireProcessTree: true); await fixture.WaitForExitAsync(); }
            }
            string errors = await errorsTask;
            if (fixture.ExitCode != 0) throw new InvalidOperationException($"Owned gateway fixture failed with exit {fixture.ExitCode}: {errors}");
        }
    }

    private static string FindRepositoryRoot()
    {
        DirectoryInfo? directory = new(AppContext.BaseDirectory);
        while (directory is not null && !File.Exists(Path.Combine(directory.FullName, "apps", "model-gateway", "server.mjs"))) directory = directory.Parent;
        return directory?.FullName ?? throw new DirectoryNotFoundException("Run the smoke test inside the repository.");
    }

    private static async Task<RetrievalIndexJob> WaitForJobAsync(IRetrievalApi api, string jobId, bool allowCancelled = false)
    {
        var timer = Stopwatch.StartNew();
        while (true)
        {
            var job = await api.GetIndexJobAsync(jobId);
            if (job.JobId != jobId) throw new InvalidDataException("The isolated index response changed its owned job identity.");
            if (job.Status is "completed" or "partial" || allowCancelled && job.Status == "cancelled") return job;
            if (job.Status is "failed" or "cancelled") throw new InvalidOperationException($"Index job {jobId} ended with {job.Status}: {job.Error}");
            if (job.Status is not ("queued" or "running" or "paused"))
                throw new InvalidDataException($"Index job {jobId} returned an unsupported status: {job.Status}.");
            if (timer.ElapsedMilliseconds > 15000)
                throw new TimeoutException($"The isolated index job did not finish: {job.Status}, {job.CompletedSources}/{job.TotalSources} sources.");
            await Task.Delay(50);
        }
    }

    private sealed record FixtureInfo(string Address, Guid ProjectId, Guid ConversationId, string GlobalSource, string ProjectSource, string Root);
}
