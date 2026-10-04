using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

public sealed record ScreenshotMessage(Guid ConversationId, Guid MessageId, string Role, IReadOnlyList<ToolActivity> Tools);
public sealed record ConversationScreenshotSource(Guid ConversationId, Guid MessageId, ToolActivity Tool)
{
    public string Identity => $"{ConversationId:D}/{MessageId:D}/{Tool.ToolCallId}/{Tool.ResultRef!.Id:D}/{Tool.ResultRef.Bytes}/{Tool.ResultRef.Sha256.ToLowerInvariant()}";
}

/// <summary>Projects completed screenshot receipts without changing formal messages or trusting model text.</summary>
public static class ConversationScreenshotSources
{
    public const long MaximumResultBytes = 8 * 1024 * 1024;

    public static bool IsScreenshotTool(string? name) => name == "computer.screenshot" ||
        (name is not null && name.StartsWith("mcp.", StringComparison.Ordinal) &&
         (name.EndsWith(".browser_take_screenshot", StringComparison.Ordinal) || name.EndsWith(".take_screenshot", StringComparison.Ordinal)));

    public static bool IsValidReference(ToolResultReference? reference) => reference is not null && reference.Id != Guid.Empty &&
        reference.Bytes is > 0 and <= MaximumResultBytes && reference.Sha256 is { Length: 64 } && reference.Sha256.All(Uri.IsHexDigit);

    public static ConversationScreenshotSource[] Collect(Guid? conversationId, IEnumerable<ScreenshotMessage> messages)
    {
        if (conversationId is null || conversationId == Guid.Empty) return [];
        var results = new List<ConversationScreenshotSource>();
        var calls = new HashSet<(Guid MessageId, string CallId)>();
        var references = new HashSet<Guid>();
        foreach (var message in messages)
        {
            if (message.ConversationId != conversationId || message.MessageId == Guid.Empty || message.Role != "assistant") continue;
            foreach (var tool in message.Tools)
            {
                if (tool is null || !IsScreenshotTool(tool.Name) || tool.Status != "completed" || !IsValidReference(tool.ResultRef) ||
                    string.IsNullOrWhiteSpace(tool.ToolCallId) || tool.ToolCallId.Length > 200 || tool.ToolCallId.Any(char.IsControl)) continue;
                if (!calls.Add((message.MessageId, tool.ToolCallId)) || !references.Add(tool.ResultRef!.Id)) continue;
                results.Add(new(conversationId.Value, message.MessageId, tool));
            }
        }
        return results.ToArray();
    }
}
