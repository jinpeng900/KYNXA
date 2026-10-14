using System.Net;
using System.Text.Json;
using System.Text.Json.Nodes;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;

internal static partial class Smoke
{
    private static readonly Guid QuoteId = Guid.Parse("09f62bc6-cc41-4821-ab16-2cba01632a85");

    private static MemoryEntry Candidate(MemoryTarget target) => Entry(target, active: false) with
    {
        Id = Guid.Parse("51af73b3-b6db-4b67-9d55-eb6a702b6f0e"), Status = MemoryStatuses.Draft,
        Source = new("user-message", "user", Chat.Id, QuoteId),
        Candidate = new("conservative-extractive-v1", "decision", new string('a', 64), [new(QuoteId, "完整用户原话 / exact quote")])
    };

    private static JsonObject DraftDocument() => JsonSerializer.SerializeToNode(
        Doc(Project, 8, Entry(Project), Candidate(Project)), new JsonSerializerOptions(JsonSerializerDefaults.Web))!.AsObject();

    private static JsonObject DraftRow(JsonObject document) => document["entries"]![1]!.AsObject();

    private static JsonObject Proposal() => new()
    {
        ["algorithm"] = "main-model-tool-v1", ["action"] = "update", ["reason"] = "New user correction",
        ["isInference"] = false, ["fingerprint"] = new string('b', 64), ["conflicts"] = new JsonArray("Old wording differs"),
        ["quotes"] = new JsonArray(new JsonObject { ["messageId"] = QuoteId, ["text"] = "完整用户原话 / exact quote" }),
        ["target"] = new JsonObject { ["id"] = EntryId, ["scope"] = Project.Scope, ["scopeId"] = Project.ScopeId,
            ["expectedRevision"] = 8, ["entryRevision"] = 1, ["identity"] = new string('c', 64) }
    };

    private static async Task DraftResponsesAsync()
    {
        JsonObject response = DraftDocument();
        using var http = new HttpClient(new Handler((_, _) => Task.FromResult(Json(response))))
            { BaseAddress = new("http://memory.test.invalid") };
        using var client = new MemoryApiClient(http);
        var mixed = await client.GetAsync(Project);
        Assert(mixed.Entries.Length == 2 && mixed.Entries[0].Status == MemoryStatuses.Confirmed &&
            mixed.Entries[1].Candidate!.Quotes[0].Text == "完整用户原话 / exact quote", "Mixed statuses and exact quotations must survive transport.");

        DraftRow(response).Remove("candidate");
        DraftRow(response)["proposal"] = Proposal();
        Assert((await client.GetAsync(Project)).Entries[1].Status == MemoryStatuses.Draft,
            "A valid typed update proposal remains a draft until explicit confirmation.");
        DraftRow(response)["proposal"]!["action"] = "delete";
        Assert((await client.GetAsync(Project)).Entries.Length == 2, "A deletion proposal must not remove its confirmed target while reading.");

        var invalidCandidates = new Action<JsonObject>[]
        {
            row => row["status"] = "unknown", row => row["active"] = true,
            row => row["source"]!["role"] = "assistant", row => row["source"]!["type"] = "manual",
            row => row["source"]!["conversationId"] = Guid.Empty, row => row["source"]!["messageId"] = Guid.Empty,
            row => row["candidate"] = null, row => row["candidate"]!["algorithm"] = "untrusted",
            row => row["candidate"]!["trigger"] = "unknown", row => row["candidate"]!["fingerprint"] = "short",
            row => row["candidate"]!["quotes"] = new JsonArray(),
            row => row["candidate"]!["quotes"]![0]!["messageId"] = Guid.Empty,
            row => row["candidate"]!["quotes"]![0]!["messageId"] = Guid.NewGuid(),
            row => row["candidate"]!["quotes"]![0]!["text"] = " ",
            row => row["candidate"]!["quotes"]![0]!["text"] = new string('x', 4001),
            row => row["candidate"]!["quotes"]![0]!["text"] = "bad\0quote",
            row => row["candidate"]!["quotes"]!.AsArray().Add(row["candidate"]!["quotes"]![0]!.DeepClone()),
            row => row["candidate"]!["batchThroughMessageId"] = QuoteId,
            row => row["candidate"]!["unknown"] = "unexpected",
            row => row["proposal"] = Proposal()
        };
        foreach (var invalidate in invalidCandidates)
        {
            response = DraftDocument(); invalidate(DraftRow(response));
            await ThrowsRejectedDraftResponseAsync(() => client.GetAsync(Project));
        }
        var invalidProposals = new Action<JsonObject>[]
        {
            proposal => proposal["algorithm"] = "untrusted", proposal => proposal["action"] = "noop",
            proposal => proposal["reason"] = " ", proposal => proposal["isInference"] = null,
            proposal => proposal["quotes"] = new JsonArray(), proposal => proposal["fingerprint"] = "short",
            proposal => proposal["target"] = null, proposal => proposal["target"]!["scopeId"] = Chat.ScopeId,
            proposal => proposal["target"]!["entryRevision"] = 0, proposal => proposal["appliedAt"] = "2026-10-10T00:00:00Z"
        };
        foreach (var invalidate in invalidProposals)
        {
            response = DraftDocument(); DraftRow(response).Remove("candidate");
            var proposal = Proposal(); invalidate(proposal); DraftRow(response)["proposal"] = proposal;
            await ThrowsRejectedDraftResponseAsync(() => client.GetAsync(Project));
        }

        response = DraftDocument();
        var candidate = DraftRow(response)["candidate"]!;
        candidate["trigger"] = "ordinary-batch"; candidate["batchThroughMessageId"] = QuoteId;
        Assert((await client.GetAsync(Project)).Entries[1].Candidate!.BatchThroughMessageId == QuoteId,
            "A valid complete-turn batch cursor must be retained.");

        var chatDocument = Doc(Chat, 9, Candidate(Chat));
        using var chatHttp = new HttpClient(new Handler((_, _) => Task.FromResult(Json(
            new ConversationMemoryResponse(Chat.Id!.Value, Project.Id, false, [chatDocument, Doc(Project, 8)])))))
            { BaseAddress = new("http://memory.test.invalid") };
        using var chatClient = new MemoryApiClient(chatHttp);
        Assert((await chatClient.GetAsync(Chat)).Entries.Single().Status == MemoryStatuses.Draft,
            "Chat responses also validate and retain their own pending entries.");
    }

    private static async Task ReviewDraftWithoutImplicitConfirmationAsync()
    {
        var current = Doc(Project, 8, Entry(Project), Candidate(Project));
        var api = new FakeApi { Get = (_, _) => Task.FromResult(current) };
        using var vm = new MemoryManagementViewModel(api);
        await vm.SelectTargetAsync(Project);
        vm.BeginEdit(current.Entries[1]);
        Assert(vm.IsDraft && !vm.CanSave && vm.CanDelete && vm.CanConfirm && !vm.HasChanges && api.Writes == 0,
            "Opening a pending candidate exposes explicit review actions without writing or confirming it.");
        vm.EditorContent = "Edited draft still needs explicit confirmation";
        vm.EditorKind = MemoryKinds.Preference;
        Assert(vm.HasChanges && vm.CanSave && !vm.CanConfirm && api.Writes == 0 &&
            current.Entries[1].Status == MemoryStatuses.Draft && current.Entries[1].Active == false,
            "Unsaved draft edits disable confirmation and leave authoritative pending source unchanged.");
        vm.BeginEdit(current.Entries[0]); vm.EditorContent = "Confirmed editing still works";
        Assert(vm.CanSave && vm.CanDelete && vm.HasChanges && !vm.IsDraft,
            "Confirmed edit controls retain their existing behavior.");
        vm.BeginNew(); vm.EditorContent = "New manual memory";
        Assert(!vm.IsDraft && vm.CanSave && vm.IsNew, "Creating manual confirmed memory remains available.");
    }

    private static async Task ThrowsRejectedDraftResponseAsync(Func<Task> action)
    {
        // Typed proposal fields can reject malformed JSON before provenance validation runs.
        // 建议的强类型字段可能在来源校验之前拒绝无效 JSON；两种拒绝均不得返回可操作草稿。
        try { await action(); }
        catch (Exception error) when (error is InvalidDataException or JsonException) { return; }
        throw new InvalidOperationException("Malformed draft provenance was accepted.");
    }
}
