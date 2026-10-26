using KYNXA.Contracts;
using System.Text.Json;

namespace KYNXA_Desktop.Services;

public sealed record ScreenshotMessage(Guid ConversationId, Guid MessageId, string Role, IReadOnlyList<ToolActivity> Tools);
public sealed record ConversationScreenshotSource(Guid ConversationId, Guid MessageId, ToolActivity Tool,
    WorkspacePresentationPriority PresentationPriority = WorkspacePresentationPriority.CatalogOnly, string? Title = null)
{
    public string TabKey => $"screenshot/{ConversationId:D}/{Tool.ResultRef!.Id:D}";
    public string Identity => $"{TabKey}/{Tool.ResultRef!.Bytes}/{Tool.ResultRef.Sha256.ToLowerInvariant()}";
}

/// <summary>
/// Projects completed screenshot receipts without changing formal messages or trusting model text.
/// 只从完成的截图回执生成展示，不改变正式消息，也不信任模型描述。
/// </summary>
public static class ConversationScreenshotSources
{
    public const long MaximumResultBytes = 8 * 1024 * 1024;
    public const long MaximumHydratedResultBytes = 44 * 1024 * 1024;
    public const int MaximumImageBytes = 32 * 1024 * 1024;
    public const long MaximumImagePixels = 64_000_000;

    // Recognize canonical capture names; loading still validates the archived image and conversation.
    // 识别正式截图能力名称；加载阶段仍核验归档图像和聊天范围，不凭模型描述显示图片。
    public static bool IsScreenshotTool(string? name) => name is "computer.screenshot" or "browser.screenshot" ||
        (name is not null && name.StartsWith("mcp.", StringComparison.Ordinal) &&
         (name.EndsWith(".browser_take_screenshot", StringComparison.Ordinal) ||
          name.EndsWith(".take_screenshot", StringComparison.Ordinal) ||
          name.EndsWith(".screenshot", StringComparison.OrdinalIgnoreCase)));

    public static bool IsValidReference(ToolResultReference? reference) => reference is not null && reference.Id != Guid.Empty &&
        reference.Bytes is > 0 and <= MaximumResultBytes && reference.Sha256 is { Length: 64 } && reference.Sha256.All(Uri.IsHexDigit);

    public static ConversationScreenshotSource[] Collect(Guid? conversationId, IEnumerable<ScreenshotMessage> messages)
    {
        if (conversationId is null || conversationId == Guid.Empty) return [];
        var results = new List<ConversationScreenshotSource>();
        var calls = new HashSet<(Guid MessageId, string CallId)>();
        var references = new Dictionary<Guid, int>();
        foreach (var message in messages)
        {
            if (message.ConversationId != conversationId || message.MessageId == Guid.Empty || message.Role != "assistant") continue;
            foreach (var tool in message.Tools)
            {
                if (tool is null || tool.Status != "completed" || !IsValidReference(tool.ResultRef) ||
                    string.IsNullOrWhiteSpace(tool.ToolCallId) || tool.ToolCallId.Length > 200 || tool.ToolCallId.Any(char.IsControl)) continue;
                if (!calls.Add((message.MessageId, tool.ToolCallId))) continue;
                ConversationScreenshotSource? source = IsScreenshotTool(tool.Name) ? new(conversationId.Value, message.MessageId, tool) :
                    PresentedSource(conversationId.Value, message.MessageId, tool);
                if (source is null) continue;
                if (references.TryGetValue(source.Tool.ResultRef!.Id, out int previous))
                {
                    if (source.PresentationPriority > results[previous].PresentationPriority && source.Tool.ResultRef == results[previous].Tool.ResultRef)
                        results[previous] = results[previous] with { PresentationPriority = source.PresentationPriority, Title = source.Title };
                    continue;
                }
                references.Add(source.Tool.ResultRef.Id, results.Count);
                results.Add(source);
            }
        }
        return results.ToArray();
    }

    private static ConversationScreenshotSource? PresentedSource(Guid conversationId, Guid messageId, ToolActivity tool)
    {
        if (tool.Name != "artifact.open" || string.IsNullOrWhiteSpace(tool.Result) || tool.Result.Length > 16384) return null;
        try
        {
            using var document = JsonDocument.Parse(tool.Result);
            var receipt = document.RootElement;
            if (String(receipt, "kind") != "screenshot" || String(receipt, "presentation") != "workspace" ||
                receipt.TryGetProperty("isError", out var error) && error.ValueKind == JsonValueKind.True ||
                !receipt.TryGetProperty("sourceResultRef", out var reference) || reference.ValueKind != JsonValueKind.Object ||
                !Guid.TryParse(String(reference, "id"), out var id) || !reference.TryGetProperty("bytes", out var bytes) ||
                bytes.ValueKind != JsonValueKind.Number || !bytes.TryGetInt64(out long count)) return null;
            var resultRef = new ToolResultReference(id, count, String(reference, "sha256") ?? "");
            string? name = String(receipt, "sourceToolName"), callId = String(receipt, "sourceToolCallId");
            if (!IsValidReference(resultRef) || string.IsNullOrWhiteSpace(name) || name.Length > 256 || name.Any(char.IsControl) ||
                string.IsNullOrWhiteSpace(callId) || callId.Length > 200 || callId.Any(char.IsControl)) return null;
            string? title = String(receipt, "title");
            if (title?.Length > 256) title = title[..256];
            return new(conversationId, messageId, tool with { Name = name, ToolCallId = callId, ResultRef = resultRef },
                WorkspacePresentationPriority.Requested, title);
        }
        catch (JsonException) { return null; }
    }

    // Only the native browser envelope may wrap image blocks; unrelated nested JSON stays opaque.
    // 只识别原生浏览器对图像的一层包装；无关嵌套 JSON 保持不透明。
    public static bool TryGetImageBlocks(JsonElement receipt, out JsonElement blocks)
    {
        blocks = default;
        if (receipt.ValueKind != JsonValueKind.Object) return false;
        if (String(receipt, "sessionId") is not null && String(receipt, "tabId") is not null &&
            receipt.TryGetProperty("value", out var browser) && browser.ValueKind == JsonValueKind.Object &&
            browser.TryGetProperty("content", out blocks) && blocks.ValueKind == JsonValueKind.Array) return true;
        return receipt.TryGetProperty("content", out blocks) && blocks.ValueKind == JsonValueKind.Array;
    }

    private static string? String(JsonElement value, string name) => value.ValueKind == JsonValueKind.Object &&
        value.TryGetProperty(name, out var field) && field.ValueKind == JsonValueKind.String ? field.GetString() : null;
}
