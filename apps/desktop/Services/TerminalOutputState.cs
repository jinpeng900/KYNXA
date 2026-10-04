using System.Text;
using System.Text.Json;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

public sealed record TerminalRunIdentity(Guid ConversationId, Guid RequestId, string ToolCallId);

/// <summary>Bounded display cache only. The gateway still owns command receipts and formal output.</summary>
public sealed class TerminalOutputState
{
    public const int MaximumOutputBytes = 256 * 1024;
    public const int MaximumRunsPerConversation = 8;
    private const int MaximumConversations = 16;
    private const int MaximumTotalBytes = 4 * 1024 * 1024;
    private readonly Dictionary<Guid, ConversationRuns> _conversations = [];
    private long _version;

    private sealed class ConversationRuns
    {
        public List<TerminalRun> Runs { get; } = [];
        public TerminalRunIdentity? Selected { get; set; }
        public bool Closed { get; set; }
        public long Touched { get; set; }
    }

    public sealed class TerminalRun(TerminalRunIdentity identity)
    {
        public TerminalRunIdentity Identity { get; } = identity;
        public string Shell { get; internal set; } = "";
        public string Script { get; internal set; } = "";
        public string Cwd { get; internal set; } = "";
        public string Output { get; internal set; } = "";
        public string Status { get; internal set; } = "running";
        public string? Code { get; internal set; }
        public int? ExitCode { get; internal set; }
        public int LastSequence { get; internal set; }
        public bool HasLiveOutput { get; internal set; }
        public bool HasReceiptOutput { get; internal set; }
        public bool Truncated { get; internal set; }
        public bool IsExecuting => Status is "running" or "approval-required";
    }

    public IReadOnlyList<TerminalRun> Runs(Guid conversationId) =>
        _conversations.TryGetValue(conversationId, out var chat) ? chat.Runs : [];

    public TerminalRun? Selected(Guid? conversationId) => conversationId is Guid id &&
        _conversations.TryGetValue(id, out var chat)
            ? chat.Runs.FirstOrDefault(run => run.Identity == chat.Selected) ?? chat.Runs.LastOrDefault() : null;

    public bool IsClosed(Guid? conversationId) => conversationId is Guid id &&
        _conversations.TryGetValue(id, out var chat) && chat.Closed;

    public bool Select(Guid conversationId, int index)
    {
        if (!_conversations.TryGetValue(conversationId, out var chat) || index < 0 || index >= chat.Runs.Count) return false;
        chat.Selected = chat.Runs[index].Identity;
        return true;
    }

    public void Close(Guid? conversationId)
    {
        if (conversationId is Guid id && _conversations.TryGetValue(id, out var chat)) chat.Closed = true;
    }

    public void Reopen(Guid? conversationId)
    {
        if (conversationId is Guid id && _conversations.TryGetValue(id, out var chat)) chat.Closed = false;
    }

    public bool Observe(TerminalRunIdentity identity, ToolActivity activity, bool live, bool requestActive = true)
    {
        if (activity.Name != "terminal.host.run" || identity.ConversationId == Guid.Empty || identity.RequestId == Guid.Empty ||
            identity.ToolCallId != activity.ToolCallId || string.IsNullOrWhiteSpace(identity.ToolCallId)) return false;
        var chat = GetConversation(identity.ConversationId);
        var run = chat.Runs.FirstOrDefault(candidate => candidate.Identity == identity);
        bool added = run is null;
        if (run is null)
        {
            run = new(identity);
            chat.Runs.Add(run);
            while (chat.Runs.Count > MaximumRunsPerConversation) chat.Runs.RemoveAt(0);
            chat.Selected = identity;
            if (live) chat.Closed = false;
        }
        if (activity.Arguments is JsonElement args && args.ValueKind == JsonValueKind.Object)
        {
            run.Shell = StringField(args, "shell", 32);
            run.Script = StringField(args, "script", 16384);
            run.Cwd = StringField(args, "cwd", 4096);
        }
        if (run.Cwd.Length == 0) run.Cwd = activity.WorkspaceRoot ?? "";
        // A reloaded interrupted reply has no stream to resume. Do not invent a completion receipt.
        run.Status = !requestActive && (activity.Status is "running" or "approval-required") ? "unknown" : activity.Status;
        run.Code = activity.Code;
        ReadReceipt(run, activity.Result);
        EnforceTotalBound(identity.ConversationId);
        return added;
    }

    public bool Append(Guid conversationId, Guid requestId, HostTerminalOutput output)
    {
        if (!HostTerminalOutputRules.IsValid(output) || !_conversations.TryGetValue(conversationId, out var chat)) return false;
        var run = chat.Runs.FirstOrDefault(candidate => candidate.Identity.RequestId == requestId && candidate.Identity.ToolCallId == output.ToolCallId);
        if (run is null || !run.IsExecuting || output.Sequence != run.LastSequence + 1) return false;
        run.LastSequence = output.Sequence;
        run.HasLiveOutput = true;
        SetOutput(run, output.Replace ? output.Text : run.Output + output.Text);
        chat.Touched = ++_version;
        EnforceTotalBound(conversationId);
        return true;
    }

    public void EndRequest(Guid conversationId, Guid requestId)
    {
        if (!_conversations.TryGetValue(conversationId, out var chat)) return;
        foreach (var run in chat.Runs.Where(run => run.Identity.RequestId == requestId && run.IsExecuting)) run.Status = "unknown";
    }

    public void Clear() => _conversations.Clear();

    public bool ApplyArchive(TerminalRunIdentity identity, JsonElement canonical)
    {
        if (!_conversations.TryGetValue(identity.ConversationId, out var chat)) return false;
        var run = chat.Runs.FirstOrDefault(candidate => candidate.Identity == identity);
        if (run is null || run.HasLiveOutput || canonical.ValueKind != JsonValueKind.Object ||
            !canonical.TryGetProperty("structuredContent", out var receipt) || receipt.ValueKind != JsonValueKind.Object ||
            !receipt.TryGetProperty("boundary", out var boundary) || boundary.ValueKind != JsonValueKind.String || boundary.GetString() != "host-terminal") return false;
        ApplyReceipt(run, receipt);
        EnforceTotalBound(identity.ConversationId);
        return run.HasReceiptOutput;
    }

    private ConversationRuns GetConversation(Guid id)
    {
        if (!_conversations.TryGetValue(id, out var chat)) _conversations[id] = chat = new();
        chat.Touched = ++_version;
        while (_conversations.Count > MaximumConversations)
        {
            var oldest = _conversations.Where(pair => pair.Key != id).MinBy(pair => pair.Value.Touched);
            _conversations.Remove(oldest.Key);
        }
        return chat;
    }

    private void EnforceTotalBound(Guid protectedId)
    {
        while (_conversations.Sum(pair => pair.Value.Runs.Sum(run => Encoding.UTF8.GetByteCount(run.Output))) > MaximumTotalBytes)
        {
            var oldest = _conversations.Where(pair => pair.Key != protectedId).MinBy(pair => pair.Value.Touched);
            if (oldest.Value is null) break; // One conversation is bounded to 8 * 256 KiB.
            _conversations.Remove(oldest.Key);
        }
    }

    private static void SetOutput(TerminalRun run, string output)
    {
        byte[] bytes = Encoding.UTF8.GetBytes(output);
        if (bytes.Length <= MaximumOutputBytes) { run.Output = output; return; }
        int offset = bytes.Length - MaximumOutputBytes;
        while (offset < bytes.Length && (bytes[offset] & 0xc0) == 0x80) offset++;
        run.Output = Encoding.UTF8.GetString(bytes, offset, bytes.Length - offset);
        run.Truncated = true;
    }

    private static string StringField(JsonElement value, string name, int maximum) =>
        value.TryGetProperty(name, out var field) && field.ValueKind == JsonValueKind.String && field.GetString() is string text
            ? text[..Math.Min(text.Length, maximum)] : "";

    private static void ReadReceipt(TerminalRun run, string? result)
    {
        if (string.IsNullOrEmpty(result) || result.Length > 65536) return;
        try
        {
            using var document = JsonDocument.Parse(result, new JsonDocumentOptions { MaxDepth = 12 });
            var value = document.RootElement;
            // Only the known receipt envelope is inspected, never _meta or arbitrary parameter objects.
            for (int depth = 0; depth < 4 && value.ValueKind == JsonValueKind.Object; depth++)
            {
                if (value.TryGetProperty("stdout", out _) || value.TryGetProperty("consoleText", out _)) break;
                if (value.TryGetProperty("output", out var nested) || value.TryGetProperty("structuredContent", out nested) ||
                    value.TryGetProperty("preview", out nested)) value = nested;
                else break;
            }
            ApplyReceipt(run, value);
        }
        catch (JsonException) { } // A preview is optional; its raw JSON is never shown as terminal text.
    }

    private static void ApplyReceipt(TerminalRun run, JsonElement receipt)
    {
        if (receipt.ValueKind != JsonValueKind.Object) return;
        if (receipt.TryGetProperty("exitCode", out var exit) && exit.ValueKind == JsonValueKind.Number && exit.TryGetInt32(out int code)) run.ExitCode = code;
        if (run.HasLiveOutput || !receipt.TryGetProperty("stdout", out _) && !receipt.TryGetProperty("consoleText", out _)) return;
        string console = StringField(receipt, "consoleText", 8 * 1024 * 1024);
        string stdout = StringField(receipt, "stdout", 8 * 1024 * 1024), stderr = StringField(receipt, "stderr", 8 * 1024 * 1024);
        SetOutput(run, console.Length > 0 ? console : stdout + (stdout.Length > 0 && stderr.Length > 0 ? "\n" : "") + stderr);
        run.HasReceiptOutput = true;
    }
}
