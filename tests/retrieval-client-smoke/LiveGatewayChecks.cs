using System.Diagnostics;
using System.Net;
using System.Net.Http.Json;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;

internal static class LiveGatewayChecks
{
    public static async Task RunAsync(string[] args)
    {
        await using var fixture = await NativeGatewayFixture.StartAsync(args);
        int checks = 0;
        void Check(bool condition, string description)
        {
            if (!condition) throw new InvalidOperationException(description);
            checks++;
        }
        try
        {
            var info = new FixtureInfo(fixture.Address.AbsoluteUri, fixture.ProjectId, fixture.ConversationId,
                fixture.GlobalSource, fixture.ProjectSource, fixture.Root);
            using var http = new HttpClient { BaseAddress = new Uri(info.Address), Timeout = TimeSpan.FromSeconds(25) };
            using var api = new RetrievalApiClient(http);
            using var initialCatalog = await http.GetFromJsonAsync<JsonDocument>("/api/conversations/catalog");
            var global = await api.GetSettingsAsync();
            Check(global.SchemaVersion == 1 && global.Local.Enabled && global.Local.Semantic == "off" && global.Local.EmbeddingProfileId == "builtin-multilingual",
                "Native settings deserialize with semantic inference disabled.");
            global = await api.SaveSettingsAsync(new(global.Revision, new(Web: global.Web with { Depth = "deep" })));
            Check(global.Web.Depth == "deep" && global.Revision == 1, "Real CAS update changes web settings immediately.");
            global = await api.SaveSettingsAsync(new(global.Revision, new(Web: global.Web with { Language = "zh-CN" })));
            Check(global.Web.Language == "zh-CN" && global.Revision == 2, "Second native CAS accepts the previous integer revision.");
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
            Check(project.Revision == 2 && project.Overrides.Web is null && project.Effective.Web.Depth == "deep", "Second project CAS restores inheritance with an integer revision.");
            Check((await api.GetProvidersAsync()).Providers.All(provider => !provider.Ready),
                "Provider presets may exist, but reading their catalog does not connect external MCP services.");

            var importedGlobal = await api.ImportSourceAsync(new("user", info.GlobalSource));
            Check(importedGlobal.Scope == "user" && importedGlobal.ProjectId is null && importedGlobal.Revision == 1 &&
                importedGlobal.ImportedCount == 1 && importedGlobal.JobId is not null, "Real source import exposes its source revision and owned job.");
            await WaitForJobAsync(api, importedGlobal.JobId!);
            var globalJob = await api.GetIndexJobAsync(importedGlobal.JobId!);
            Check(globalJob.Status == "completed" && globalJob.Coverage is { Lexical: > 0, Complete: true },
                "Semantic-disabled native preparation completes real lexical publication without a model.");
            var importedProject = await api.ImportSourceAsync(new("project", info.ProjectSource, info.ProjectId));
            Check(importedProject.ProjectId == info.ProjectId && importedProject.Scope == "project", "Real project source retains its stable owner.");
            await WaitForJobAsync(api, importedProject.JobId!);
            var importedOther = await api.ImportSourceAsync(new("project", fixture.OtherSource, fixture.OtherProjectId));
            await WaitForJobAsync(api, importedOther.JobId!);
            var globalSources = await api.GetSourcesAsync();
            var visibleSources = await api.GetSourcesAsync(info.ProjectId);
            Check(globalSources.Sources.Length == 1 && globalSources.Sources.All(source => source.Scope == "user"), "Global source lists exclude project sources.");
            Check(visibleSources.Sources.Length == 2 && visibleSources.Sources.Any(source => source.Scope == "user") &&
                visibleSources.Sources.Any(source => source.ProjectId == info.ProjectId) &&
                visibleSources.Sources.All(source => source.Id != importedOther.Id), "Project source lists include global sources and exclude another project.");

            var status = await api.GetStatusAsync();
            Check(status.Backend == "sqlite" && status.SourceCount == 3 && status.ChunkCount >= 3,
                "Real index status uses the public source and chunk count fields.");
            Check(status.Embedding.Loaded == false && status.Embedding.Dimensions == 384,
                "Semantic-disabled checks do not load the actual embedding model; asset availability is independent.");
            using (var found = await SearchAsync(http, info.ConversationId, "PROJECT_RETRIEVAL_FIXTURE"))
                Check(found.RootElement.GetProperty("items").EnumerateArray().Any(item => item.GetProperty("sourceId").GetString() == importedProject.Id),
                    $"Actual scoped lexical search finds the project source {importedProject.Id}. Native result: {found.RootElement.GetRawText()}");
            using (var hidden = await SearchAsync(http, info.ConversationId, "OTHER_SCOPE_PRIVATE_FIXTURE"))
                Check(hidden.RootElement.GetProperty("items").EnumerateArray().All(item => item.GetProperty("sourceId").GetString() != importedOther.Id),
                    "Actual search excludes another project's private source.");
            using (var invalidDomain = await http.PostAsJsonAsync("/api/retrieval/search",
                new { conversationId = info.ConversationId, query = "PROJECT_RETRIEVAL_FIXTURE", domain = "document" }))
                Check(invalidDomain.StatusCode == HttpStatusCode.BadRequest, "Invalid search domains are rejected instead of silently excluding all documents.");
            await CheckMemoryAsync(http, info.ProjectId, info.ConversationId, fixture.OtherProjectId, Check);
            await api.DeleteSourceAsync(importedProject.Id, importedProject.Revision);
            Check(!(await api.GetSourcesAsync(info.ProjectId)).Sources.Any(source => source.Id == importedProject.Id), "Real removal revokes the project source.");
            await api.DeleteSourceAsync(importedGlobal.Id);
            Check((await api.GetSourcesAsync()).Sources.Length == 0, "Omitted optional delete revision does not become a null conflict.");
            var rebuild = await api.RebuildIndexAsync(info.ProjectId);
            Check(rebuild.Status is "queued" or "running" or "completed", "Manual rebuilding returns a valid durable job.");
            try
            {
                var cancellation = await api.CancelIndexJobAsync(rebuild.JobId);
                Check(cancellation.JobId == rebuild.JobId && cancellation.Status == "cancelled", "Cancellation acknowledges the selected durable job.");
                await WaitForJobAsync(api, rebuild.JobId, allowCancelled: true);
            }
            catch (GatewayApiException error) when (error.StatusCode == HttpStatusCode.Conflict)
            {
                Check((await api.GetIndexJobAsync(rebuild.JobId)).Status is "completed" or "partial", "A completion/cancel race preserves the terminal job.");
            }
            using var finalCatalog = await http.GetFromJsonAsync<JsonDocument>("/api/conversations/catalog");
            Check(initialCatalog!.RootElement.GetRawText() == finalCatalog!.RootElement.GetRawText(),
                "Settings, import, indexing and removal never create or modify a conversation.");
            Console.WriteLine($"Native Rust retrieval live HTTP passed: {checks} checks; real C# DTOs, SQLite, source scope, settings CAS and memory; semantic disabled, no model API.");
        }
        catch (Exception error)
        {
            Console.Error.WriteLine($"Native live stopped after {checks} successful checks.");
            Console.Error.WriteLine(error);
            throw;
        }
    }

    private static async Task<JsonDocument> SearchAsync(HttpClient http, Guid conversation, string query)
    {
        using var response = await http.PostAsJsonAsync("/api/retrieval/search", new { conversationId = conversation, query, domain = "knowledge", maximumTokens = 4096 });
        await EnsureSuccessAsync(response);
        return await response.Content.ReadFromJsonAsync<JsonDocument>() ?? throw new InvalidDataException("Native search response is missing.");
    }
    private static async Task<MemoryScopeDocument> ReadMemoryAsync(HttpClient http, string path)
    {
        using var response = await http.GetAsync(path);
        await EnsureSuccessAsync(response);
        return await response.Content.ReadFromJsonAsync<MemoryScopeDocument>() ?? throw new InvalidDataException("Memory document is missing.");
    }
    private static async Task<MemoryScopeDocument> WriteMemoryAsync(HttpClient http, HttpMethod method, string path, object input)
    {
        using var request = new HttpRequestMessage(method, path) { Content = JsonContent.Create(input) };
        using var response = await http.SendAsync(request);
        await EnsureSuccessAsync(response);
        return await response.Content.ReadFromJsonAsync<MemoryScopeDocument>() ?? throw new InvalidDataException("Memory document is missing.");
    }
    private static async Task EnsureSuccessAsync(HttpResponseMessage response)
    {
        if (!response.IsSuccessStatusCode) throw new InvalidOperationException($"Native fixture HTTP {(int)response.StatusCode}: {await response.Content.ReadAsStringAsync()}");
    }
    private static async Task CheckMemoryAsync(HttpClient http, Guid project, Guid chat, Guid otherProject, Action<bool, string> check)
    {
        // Decode actual native HTTP using shared production DTOs, independently from recording-handler checks.
        // 使用正式共享 DTO 解码真实原生 HTTP，与记录处理器模拟成绩分开报告。
        var user = await ReadMemoryAsync(http, "/api/memory/user");
        user = await WriteMemoryAsync(http, HttpMethod.Post, "/api/memory/user", new MemoryCreateRequest("user", "Synthetic global preference", "preference", user.Revision));
        check(user.Revision == 1 && user.Entries.Single().Scope == "user", "Actual user memory create preserves scope and integer revision.");
        Guid entryId = user.Entries.Single().Id;
        user = await WriteMemoryAsync(http, HttpMethod.Patch, $"/api/memory/user/{entryId:D}",
            new MemoryUpdateRequest("user", "Updated synthetic preference", "preference", user.Revision));
        check(user.Revision == 2 && user.Entries.Single().Content == "Updated synthetic preference", "Second actual memory CAS updates the current entry.");
        using (var stale = await http.PostAsJsonAsync("/api/memory/user", new MemoryCreateRequest("user", "Stale", "fact", 0)))
            check(stale.StatusCode == HttpStatusCode.Conflict, "Stale memory CAS is rejected with HTTP 409.");
        var work = await ReadMemoryAsync(http, $"/api/projects/{project:D}/memory");
        work = await WriteMemoryAsync(http, HttpMethod.Post, $"/api/projects/{project:D}/memory", new MemoryCreateRequest("project", "Synthetic project fact", "fact", work.Revision));
        check(work.ScopeId == project.ToString("D") && work.Entries.Single().Scope == "project", "Actual project memory retains its stable owner.");
        var local = await WriteMemoryAsync(http, HttpMethod.Post, $"/api/conversations/{chat:D}/memory", new MemoryCreateRequest("chat", "Synthetic chat fact", "fact", 0));
        check(local.ScopeId == chat.ToString("D") && local.Entries.Single().Source.ConversationId == chat, "Chat memory is bound to its actual conversation.");
        using (var response = await http.GetAsync($"/api/conversations/{chat:D}/memory"))
        {
            await EnsureSuccessAsync(response);
            var snapshot = await response.Content.ReadFromJsonAsync<ConversationMemoryResponse>() ?? throw new InvalidDataException("Memory snapshot is missing.");
            check(snapshot.ConversationId == chat && snapshot.ProjectId == project && snapshot.Scopes.Length == 3 &&
                snapshot.Scopes.All(scope => scope.ScopeId != otherProject.ToString("D")), "Chat DTO includes only authorized user/work/chat memory.");
        }
        check((await ReadMemoryAsync(http, $"/api/projects/{otherProject:D}/memory")).Entries.Length == 0, "Other project memory remains independent.");
        user = await WriteMemoryAsync(http, HttpMethod.Delete, $"/api/memory/user/{entryId:D}", new MemoryDeleteRequest("user", user.Revision));
        check(user.Revision == 3 && user.Entries.Length == 0, "Memory deletion advances revision and removes the entry.");
    }
    private static async Task WaitForJobAsync(IRetrievalApi api, string jobId, bool allowCancelled = false)
    {
        var timer = Stopwatch.StartNew();
        while (true)
        {
            var job = await api.GetIndexJobAsync(jobId);
            if (job.Status is "completed" or "partial" || allowCancelled && job.Status == "cancelled") return;
            if (job.Status is "failed" or "cancelled") throw new InvalidOperationException($"Index job {jobId} ended with {job.Status}: {job.Error}");
            if (timer.ElapsedMilliseconds > 15000) throw new TimeoutException("The isolated index job did not finish.");
            await Task.Delay(50);
        }
    }

    private sealed record FixtureInfo(string Address, Guid ProjectId, Guid ConversationId, string GlobalSource, string ProjectSource, string Root);
}
