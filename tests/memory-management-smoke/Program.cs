using System.Net;
using System.Net.Http.Json;
using System.Text;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;

await Smoke.RunAsync();

internal static class Smoke
{
    private static readonly MemoryTarget Chat = new(MemoryScopes.Chat, Guid.Parse("b168304f-d69b-49b9-b97a-6924cc40b1a4"), "Example chat");
    private static readonly MemoryTarget Project = new(MemoryScopes.Project, Guid.Parse("d8623068-8b2f-4862-baaa-7480cb18c970"), "Example work");
    private static readonly MemoryTarget User = new(MemoryScopes.User, null, "Global memory");
    private static readonly Guid EntryId = Guid.Parse("c97f2900-6a2c-4087-bd81-a4790fb98bb2");
    private static int _passed;

    public static async Task RunAsync()
    {
        await CheckAsync("HTTP scope paths and status-bearing GETs", HttpReadsAsync);
        await CheckAsync("HTTP manual CRUD uses scope revision and DELETE JSON", HttpWritesAsync);
        await CheckAsync("HTTP errors retain status and machine code", HttpErrorsAsync);
        await CheckAsync("content validation blocks invalid requests", ValidationAsync);
        await CheckAsync("HTTP cancellation reaches the injected handler", HttpCancellationAsync);
        await CheckAsync("null draft target performs no I/O", DraftAsync);
        await CheckAsync("CRUD refreshes source status and uses scope revision", CrudAsync);
        await CheckAsync("409 preserves form and requires explicit next save", ConflictAsync);
        await CheckAsync("409 refresh failure blocks writes and preserves both errors", ConflictReloadFailureAsync);
        await CheckAsync("committed create reload failure cannot duplicate on retry", CommittedReloadFailureAsync);
        await CheckAsync("cancel after committed create preserves its identity for refresh", CommittedCancellationAsync);
        await CheckAsync("uncertain create cannot blindly repeat", UnknownCreateAsync);
        await CheckAsync("server failure during create requires outcome reconciliation", ServerCreateFailureAsync);
        await CheckAsync("refresh updates clean editor while preserving pending edits", EditorRefreshAsync);
        await CheckAsync("late scope reads cannot replace current state", SwitchAsync);
        await CheckAsync("late scope writes do not reload another target", SwitchDuringWriteAsync);
        await CheckAsync("close cancels and ignores late results", CloseAsync);
        await CheckAsync("external cancellation rejects handlers that ignore the token", ExternalCancellationAsync);
        Console.WriteLine($"Memory management smoke passed: {_passed} checks.");
    }

    private static async Task CheckAsync(string name, Func<Task> test)
    {
        await test();
        _passed++;
        Console.WriteLine($"PASS {name}");
    }

    private static MemoryEntry Entry(MemoryTarget target, string content = "Example memory", long revision = 1, bool active = true) =>
        new(EntryId, target.Scope, target.ScopeId, content, MemoryKinds.Fact, "confirmed",
            new("manual", "user"), revision, DateTimeOffset.Parse("2026-09-01T00:00:00Z"),
            DateTimeOffset.Parse("2026-09-01T00:00:00Z"), active, true, false);

    private static MemoryScopeDocument Doc(MemoryTarget target, long revision, params MemoryEntry[] entries) =>
        new(1, target.Scope, target.ScopeId, revision, entries, []);

    private static HttpResponseMessage Json(object value, HttpStatusCode status = HttpStatusCode.OK) =>
        new(status) { Content = JsonContent.Create(value) };

    private static void Assert(bool condition, string message)
    {
        if (!condition) throw new InvalidOperationException(message);
    }

    private static async Task<T> ThrowsAsync<T>(Func<Task> action) where T : Exception
    {
        try { await action(); }
        catch (T error) { return error; }
        throw new InvalidOperationException($"Expected {typeof(T).Name}.");
    }

    private static async Task HttpReadsAsync()
    {
        var paths = new List<string>();
        using var http = new HttpClient(new Handler((request, _) =>
        {
            paths.Add(request.RequestUri!.AbsolutePath);
            object value = request.RequestUri.AbsolutePath.Contains("conversations")
                ? new ConversationMemoryResponse(Chat.Id!.Value, Project.Id, false,
                    [Doc(Chat, 11, Entry(Chat)), Doc(Project, 45), Doc(User, 8)])
                : request.RequestUri.AbsolutePath.Contains("projects") ? Doc(Project, 45, Entry(Project)) : Doc(User, 8, Entry(User));
            return Task.FromResult(Json(value));
        })) { BaseAddress = new("http://memory.test.invalid") };
        using var client = new MemoryApiClient(http);
        Assert((await client.GetAsync(Chat)).Revision == 11, "Chat scope must be selected from scopes.");
        Assert((await client.GetAsync(Project)).Revision == 45, "Project scope missing.");
        Assert((await client.GetAsync(User)).Entries[0].Source.ConversationId is null, "Independent manual memory must not invent a chat.");
        Assert(paths.SequenceEqual([$"/api/conversations/{Chat.Id:D}/memory", $"/api/projects/{Project.Id:D}/memory", "/api/memory/user"]), "Unexpected management GET paths.");
    }

    private static async Task HttpWritesAsync()
    {
        var calls = new List<(string Method, string Path, JsonElement Body)>();
        using var http = new HttpClient(new Handler(async (request, token) =>
        {
            var body = await request.Content!.ReadFromJsonAsync<JsonElement>(token);
            calls.Add((request.Method.Method, request.RequestUri!.AbsolutePath, body));
            var target = request.RequestUri.AbsolutePath.Contains("projects") ? Project : request.RequestUri.AbsolutePath.Contains("conversations") ? Chat : User;
            return Json(Doc(target, 43, Entry(target) with { Active = null, SourceAvailable = null, SourceArchived = null }));
        })) { BaseAddress = new("http://memory.test.invalid") };
        using var client = new MemoryApiClient(http);
        await client.CreateAsync(User, new(User.Scope, "Example global preference", MemoryKinds.Preference, 42));
        await client.UpdateAsync(Project, EntryId, new(Project.Scope, "Example work rule", MemoryKinds.Decision, 42));
        await client.DeleteAsync(Chat, EntryId, new(Chat.Scope, 42));
        Assert(calls[0].Method == "POST" && calls[0].Path == "/api/memory/user", "Global create path incorrect.");
        Assert(calls[1].Method == "PATCH" && calls[1].Path == $"/api/projects/{Project.Id:D}/memory/{EntryId:D}", "Work edit path incorrect.");
        Assert(calls[2].Method == "DELETE" && calls[2].Path == $"/api/conversations/{Chat.Id:D}/memory/{EntryId:D}", "Chat delete path incorrect.");
        Assert(calls.All(call => call.Body.GetProperty("expectedRevision").GetInt64() == 42), "Scope revision must travel on every write.");
        Assert(calls[2].Body.GetProperty("scope").GetString() == Chat.Scope, "DELETE requires a JSON body including scope.");
        Assert(!calls[0].Body.TryGetProperty("source", out _), "Manual management must not fabricate a message source.");
    }

    private static async Task HttpErrorsAsync()
    {
        foreach (var status in new[] { HttpStatusCode.BadRequest, HttpStatusCode.NotFound, HttpStatusCode.Conflict, HttpStatusCode.InternalServerError })
        {
            using var http = new HttpClient(new Handler((_, _) => Task.FromResult(Json(new { error = "Example failure", code = "EXAMPLE_CODE" }, status))))
                { BaseAddress = new("http://memory.test.invalid") };
            using var client = new MemoryApiClient(http);
            var error = await ThrowsAsync<GatewayApiException>(() => client.GetAsync(User));
            Assert(error.StatusCode == status && error.ErrorCode == "EXAMPLE_CODE", "HTTP error identity was lost.");
        }
        using var textHttp = new HttpClient(new Handler((_, _) => Task.FromResult(new HttpResponseMessage(HttpStatusCode.BadGateway)
            { Content = new StringContent("<html>private raw body</html>", Encoding.UTF8, "text/html") }))) { BaseAddress = new("http://memory.test.invalid") };
        using var textClient = new MemoryApiClient(textHttp);
        var textError = await ThrowsAsync<GatewayApiException>(() => textClient.GetAsync(User));
        Assert(textError.StatusCode == HttpStatusCode.BadGateway && !textError.Message.Contains("private raw body"), "Non-JSON failure body must not be exposed.");
    }

    private static async Task ValidationAsync()
    {
        int requests = 0;
        using var http = new HttpClient(new Handler((_, _) => { requests++; return Task.FromResult(Json(Doc(User, 1))); }))
            { BaseAddress = new("http://memory.test.invalid") };
        using var client = new MemoryApiClient(http);
        foreach (string content in new[] { " ", new string('a', 4001), "a\0b" })
            await ThrowsAsync<ArgumentException>(() => client.CreateAsync(User, new(User.Scope, content, MemoryKinds.Fact, 0)));
        await ThrowsAsync<ArgumentException>(() => client.CreateAsync(User, new(User.Scope, "Example", "unsupported", 0)));
        await ThrowsAsync<ArgumentException>(() => client.CreateAsync(User, new(MemoryScopes.Chat, "Example", MemoryKinds.Fact, 0)));
        await ThrowsAsync<ArgumentException>(() => client.GetAsync(new(MemoryScopes.Chat, Guid.Empty, "Draft")));
        Assert(requests == 0, "Invalid requests must not reach HTTP.");
        Assert(MemoryInputValidation.Validate(new string('a', 4000), MemoryKinds.Decision) == MemoryInputError.None, "4000 characters must be accepted.");
        Assert(MemoryInputValidation.Validate(string.Concat(Enumerable.Repeat("😀", 2001)), MemoryKinds.Fact) == MemoryInputError.ContentTooLong, "Gateway limit counts UTF-16 code units.");
    }

    private static async Task HttpCancellationAsync()
    {
        using var cancellation = new CancellationTokenSource();
        CancellationToken observed = default;
        using var http = new HttpClient(new Handler(async (_, token) =>
        {
            observed = token;
            await Task.Delay(Timeout.Infinite, token);
            return Json(Doc(User, 0));
        })) { BaseAddress = new("http://memory.test.invalid") };
        using var client = new MemoryApiClient(http);
        var read = client.GetAsync(User, cancellation.Token);
        cancellation.Cancel();
        await ThrowsAsync<OperationCanceledException>(() => read);
        Assert(observed.IsCancellationRequested, "Transport token must be cancelled.");
    }

    private static async Task DraftAsync()
    {
        var api = new FakeApi();
        using var vm = new MemoryManagementViewModel(api);
        await vm.SelectTargetAsync(null);
        vm.BeginNew();
        vm.EditorContent = "Draft that must remain in memory";
        Assert(await vm.SaveAsync() == MemoryOperationResult.NotReady && api.Reads == 0 && api.Writes == 0, "A draft target must not create a chat or memory.");
    }

    private static async Task CrudAsync()
    {
        var current = Doc(Project, 20, Entry(Project, revision: 2));
        var createdId = Guid.NewGuid();
        var api = new FakeApi { Get = (_, _) => Task.FromResult(current) };
        api.Create = (target, input, _) =>
        {
            Assert(input.ExpectedRevision == 20 && input.Scope == Project.Scope, "Create must use scope revision, not entry revision.");
            current = Doc(target, 21, Entry(target), Entry(target, input.Content, active: false) with { Id = createdId });
            return Task.FromResult(current with { Entries = current.Entries.Select(entry => entry with { Active = null }).ToArray() });
        };
        api.Update = (target, id, input, _) =>
        {
            Assert(id == createdId && input.ExpectedRevision == 21, "Update must use the refreshed scope revision.");
            current = Doc(target, 22, Entry(target), Entry(target, input.Content, revision: 3) with { Id = createdId });
            return Task.FromResult(current);
        };
        api.Delete = (target, id, input, _) =>
        {
            Assert(id == createdId && input.ExpectedRevision == 22, "Delete must use the scope revision.");
            current = Doc(target, 23, Entry(target));
            return Task.FromResult(current);
        };
        using var vm = new MemoryManagementViewModel(api);
        await vm.SelectTargetAsync(Project);
        vm.EditorContent = "Example added memory";
        Assert(await vm.SaveAsync() == MemoryOperationResult.Success, "Create failed.");
        Assert(api.Reads == 2 && vm.SelectedEntry?.Active == false && !vm.HasChanges, "Create must GET status and accept the backend editor baseline.");
        vm.EditorContent = "Example edited memory";
        Assert(await vm.SaveAsync() == MemoryOperationResult.Success && vm.ScopeRevision == 22, "Update failed.");
        Assert(await vm.DeleteAsync() == MemoryOperationResult.Success && vm.Entries.Count == 1 && vm.IsNew, "Delete must refresh the list and reset only its editor.");
        Assert(api.Reads == 4 && api.Writes == 3, "CRUD must perform one read after every write.");
    }

    private static async Task ConflictAsync()
    {
        var current = Doc(Chat, 5, Entry(Chat, "Old content"));
        var api = new FakeApi { Get = (_, _) => Task.FromResult(current) };
        api.Update = (target, id, input, _) =>
        {
            if (input.ExpectedRevision == 5)
            {
                current = Doc(Chat, 6, Entry(Chat, "Other client's change"));
                throw new GatewayApiException("Conflict", HttpStatusCode.Conflict, "MEMORY_CONFLICT");
            }
            Assert(input.ExpectedRevision == 6 && input.Content == "My pending edit", "Explicit retry must submit the preserved draft and new revision.");
            current = Doc(target, 7, Entry(target, input.Content));
            return Task.FromResult(current);
        };
        using var vm = new MemoryManagementViewModel(api);
        await vm.SelectTargetAsync(Chat);
        vm.BeginEdit(vm.Entries[0]);
        vm.EditorContent = "My pending edit";
        Assert(await vm.SaveAsync() == MemoryOperationResult.Conflict, "409 must be reported as conflict.");
        Assert(vm.EditorContent == "My pending edit" && vm.Entries[0].Content == "Other client's change" && vm.ScopeRevision == 6 && vm.CanSave, "Conflict must preserve editor while loading authoritative list.");
        Assert(api.Writes == 1 && api.Reads == 2 && vm.Status == MemoryManagementStatus.Conflict && vm.Error is GatewayApiException { ErrorCode: "MEMORY_CONFLICT" }, "Conflict must not auto-save.");
        Assert(await vm.SaveAsync() == MemoryOperationResult.Success && api.Writes == 2, "Explicit save must remain available.");
    }

    private static async Task ConflictReloadFailureAsync()
    {
        var api = new FakeApi();
        api.Get = (_, _) => api.Reads == 1 ? Task.FromResult(Doc(User, 3, Entry(User))) : throw new HttpRequestException("Example reload failure");
        api.Update = (_, _, _, _) => throw new GatewayApiException("Conflict", HttpStatusCode.Conflict, "MEMORY_CONFLICT");
        using var vm = new MemoryManagementViewModel(api);
        await vm.SelectTargetAsync(User);
        vm.BeginEdit(vm.Entries[0]);
        vm.EditorContent = "Pending";
        Assert(await vm.SaveAsync() == MemoryOperationResult.Conflict, "Conflict result missing.");
        Assert(vm.EditorContent == "Pending" && !vm.IsLoaded && !vm.CanSave && vm.RefreshError is HttpRequestException && vm.Error is GatewayApiException, "Failed conflict reload must preserve form and both errors.");
    }

    private static async Task CommittedReloadFailureAsync()
    {
        var current = Doc(User, 0);
        var api = new FakeApi();
        api.Get = (_, _) => api.Reads == 2 ? throw new HttpRequestException("Example reload failure") : Task.FromResult(current);
        api.Create = (target, input, _) => Task.FromResult(current = Doc(target, 1, Entry(target, input.Content)));
        using var vm = new MemoryManagementViewModel(api);
        await vm.SelectTargetAsync(User);
        vm.EditorContent = "Committed preference";
        Assert(await vm.SaveAsync() == MemoryOperationResult.Failed && vm.Status == MemoryManagementStatus.SavedRefreshFailed, "Committed save must report reload failure.");
        Assert(await vm.SaveAsync() == MemoryOperationResult.NotReady && api.Writes == 1, "Retry before reload must not duplicate create.");
        await vm.RefreshAsync();
        Assert(vm.SelectedEntry?.Id == EntryId && !vm.IsNew && !vm.HasChanges && vm.Status == MemoryManagementStatus.Saved, "Successful reload must accept the committed entry.");
    }

    private static async Task UnknownCreateAsync()
    {
        var current = Doc(User, 0);
        var api = new FakeApi { Get = (_, _) => Task.FromResult(current) };
        api.Create = (target, input, _) =>
        {
            current = Doc(target, 1, Entry(target, input.Content));
            throw new HttpRequestException("Example lost response");
        };
        using var vm = new MemoryManagementViewModel(api);
        await vm.SelectTargetAsync(User);
        vm.EditorContent = "Possibly committed";
        Assert(await vm.SaveAsync() == MemoryOperationResult.Failed && vm.Status == MemoryManagementStatus.WriteOutcomeUnknown, "Unknown create outcome needs explicit status.");
        await vm.RefreshAsync();
        Assert(vm.Entries.Count == 1 && vm.EditorContent == "Possibly committed" && !vm.CanSave && api.Writes == 1, "Refresh must not enable blind duplicate create.");
        vm.BeginEdit(vm.Entries[0]);
        Assert(!vm.IsNew && !vm.HasChanges, "User can explicitly inspect the committed entry.");
    }

    private static async Task CommittedCancellationAsync()
    {
        var current = Doc(User, 0);
        using var cancellation = new CancellationTokenSource();
        var api = new FakeApi { Get = (_, _) => Task.FromResult(current) };
        api.Create = (target, input, _) =>
        {
            current = Doc(target, 1, Entry(target, input.Content));
            cancellation.Cancel();
            return Task.FromResult(current);
        };
        using var vm = new MemoryManagementViewModel(api);
        await vm.SelectTargetAsync(User);
        vm.EditorContent = "Committed before cancellation";
        Assert(await vm.SaveAsync(cancellation.Token) == MemoryOperationResult.Cancelled && !vm.IsLoaded &&
            vm.Status == MemoryManagementStatus.SavedRefreshFailed, "A cancelled known commit must still require a reload.");
        await vm.RefreshAsync();
        Assert(!vm.IsNew && !vm.HasChanges && vm.SelectedEntry?.Id == EntryId && api.Writes == 1 && !vm.CanSave,
            "Reload must reconcile a known commit even if cancellation arrived before its first read.");
    }

    private static async Task SwitchAsync()
    {
        var late = new TaskCompletionSource<MemoryScopeDocument>();
        CancellationToken token = default;
        var api = new FakeApi { Get = (target, cancellation) =>
        {
            if (target == Chat) { token = cancellation; return late.Task; }
            return Task.FromResult(Doc(User, 9, Entry(User)));
        } };
        using var vm = new MemoryManagementViewModel(api);
        var old = vm.SelectTargetAsync(Chat);
        await vm.SelectTargetAsync(User);
        late.SetResult(Doc(Chat, 1, Entry(Chat)));
        await old;
        Assert(token.IsCancellationRequested && vm.Target == User && vm.ScopeRevision == 9 && vm.Entries.All(entry => entry.Scope == User.Scope) && !vm.IsBusy, "Late scope read polluted current state.");
    }

    private static async Task ServerCreateFailureAsync()
    {
        var api = new FakeApi { Get = (_, _) => Task.FromResult(Doc(User, 0)),
            Create = (_, _, _) => throw new GatewayApiException("Example server error", HttpStatusCode.InternalServerError, "EXAMPLE_SERVER_ERROR") };
        using var vm = new MemoryManagementViewModel(api);
        await vm.SelectTargetAsync(User);
        vm.EditorContent = "Uncertain after server error";
        Assert(await vm.SaveAsync() == MemoryOperationResult.Failed && vm.Status == MemoryManagementStatus.WriteOutcomeUnknown &&
            vm.Error is GatewayApiException { ErrorCode: "EXAMPLE_SERVER_ERROR" }, "A 5xx response cannot prove that create did not commit.");
        await vm.RefreshAsync();
        Assert(!vm.CanSave && api.Writes == 1, "Refresh alone must not blindly repeat an uncertain create.");
    }

    private static async Task EditorRefreshAsync()
    {
        var current = Doc(Chat, 2, Entry(Chat, "Initial"));
        var api = new FakeApi { Get = (_, _) => Task.FromResult(current) };
        using var vm = new MemoryManagementViewModel(api);
        await vm.SelectTargetAsync(Chat);
        vm.BeginEdit(vm.Entries[0]);
        current = Doc(Chat, 3, Entry(Chat, "External edit"));
        await vm.RefreshAsync();
        Assert(vm.EditorContent == "External edit" && !vm.HasChanges, "Clean editor should reflect current backend entry.");
        vm.EditorContent = "Pending local edit";
        current = Doc(Chat, 4, Entry(Chat, "Second external edit"));
        await vm.RefreshAsync();
        Assert(vm.EditorContent == "Pending local edit" && vm.HasChanges && vm.Entries[0].Content == "Second external edit", "Refreshing must preserve unsaved editor content.");
    }

    private static async Task SwitchDuringWriteAsync()
    {
        var late = new TaskCompletionSource<MemoryScopeDocument>();
        var api = new FakeApi { Get = (target, _) => Task.FromResult(Doc(target, 2)), Create = (_, _, _) => late.Task };
        using var vm = new MemoryManagementViewModel(api);
        await vm.SelectTargetAsync(Chat);
        vm.EditorContent = "Old target draft";
        var write = vm.SaveAsync();
        await vm.SelectTargetAsync(User);
        late.SetResult(Doc(Chat, 3, Entry(Chat)));
        Assert(await write == MemoryOperationResult.Superseded && api.Reads == 2 && vm.Target == User && vm.EditorContent.Length == 0 && vm.ScopeRevision == 2, "Late save must not reload or edit the next target.");
    }

    private static async Task CloseAsync()
    {
        var late = new TaskCompletionSource<MemoryScopeDocument>();
        CancellationToken token = default;
        var api = new FakeApi { Get = (_, cancellation) => { token = cancellation; return late.Task; } };
        var vm = new MemoryManagementViewModel(api);
        int notifications = 0;
        vm.PropertyChanged += (_, _) => notifications++;
        var read = vm.SelectTargetAsync(User);
        vm.Dispose();
        int before = notifications;
        late.SetResult(Doc(User, 3, Entry(User)));
        await read;
        Assert(token.IsCancellationRequested && api.Disposed && notifications == before && vm.Entries.Count == 0 && !vm.CanSave, "Disposed editor must ignore late results and release its API.");
    }

    private static async Task ExternalCancellationAsync()
    {
        var late = new TaskCompletionSource<MemoryScopeDocument>();
        var api = new FakeApi { Get = (_, _) => late.Task };
        using var vm = new MemoryManagementViewModel(api);
        using var cancellation = new CancellationTokenSource();
        var read = vm.SelectTargetAsync(User, cancellation.Token);
        cancellation.Cancel();
        late.SetResult(Doc(User, 9, Entry(User)));
        await read;
        Assert(!vm.IsLoaded && !vm.IsBusy && vm.Entries.Count == 0 && vm.Status == MemoryManagementStatus.Cancelled, "Cancelled read must not apply an ignored-token response.");
    }

    private sealed class Handler(Func<HttpRequestMessage, CancellationToken, Task<HttpResponseMessage>> send) : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken) => send(request, cancellationToken);
    }

    private sealed class FakeApi : IMemoryApi, IDisposable
    {
        public int Reads { get; private set; }
        public int Writes { get; private set; }
        public bool Disposed { get; private set; }
        public Func<MemoryTarget, CancellationToken, Task<MemoryScopeDocument>> Get { get; set; } = (_, _) => throw new InvalidOperationException("Unexpected read.");
        public Func<MemoryTarget, MemoryCreateRequest, CancellationToken, Task<MemoryScopeDocument>> Create { get; set; } = (_, _, _) => throw new InvalidOperationException("Unexpected create.");
        public Func<MemoryTarget, Guid, MemoryUpdateRequest, CancellationToken, Task<MemoryScopeDocument>> Update { get; set; } = (_, _, _, _) => throw new InvalidOperationException("Unexpected update.");
        public Func<MemoryTarget, Guid, MemoryDeleteRequest, CancellationToken, Task<MemoryScopeDocument>> Delete { get; set; } = (_, _, _, _) => throw new InvalidOperationException("Unexpected delete.");
        public Task<MemoryScopeDocument> GetAsync(MemoryTarget target, CancellationToken cancellationToken = default) { Reads++; return Get(target, cancellationToken); }
        public Task<MemoryScopeDocument> CreateAsync(MemoryTarget target, MemoryCreateRequest request, CancellationToken cancellationToken = default) { Writes++; return Create(target, request, cancellationToken); }
        public Task<MemoryScopeDocument> UpdateAsync(MemoryTarget target, Guid entryId, MemoryUpdateRequest request, CancellationToken cancellationToken = default) { Writes++; return Update(target, entryId, request, cancellationToken); }
        public Task<MemoryScopeDocument> DeleteAsync(MemoryTarget target, Guid entryId, MemoryDeleteRequest request, CancellationToken cancellationToken = default) { Writes++; return Delete(target, entryId, request, cancellationToken); }
        public void Dispose() => Disposed = true;
    }
}
