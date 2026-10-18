using System.Text.Json;
using System.Text.RegularExpressions;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Saved files enter the catalog; explicit display and delivered outputs receive automatic presentation priority.
/// 保存文件进入目录；明确展示和实际交付的成果才获得自动展示优先级。
/// </summary>
public static class ConversationWorkspaceSources
{
    private static readonly Regex DeliveryLinks = new(@"\]\(\s*(?:<(?<target>[^>\r\n]+)>|(?<target>[^\s)\r\n]+))(?:\s+""[^""\r\n]*"")?\s*\)", RegexOptions.CultureInvariant);

    public static bool IsResourceTool(string? name) => name is "artifact.open" or "filesystem.write" or "filesystem.edit"
        or "browser.open" or "computer.launch";

    public static ConversationWorkTab? FromTool(ToolActivity tool, JsonElement receipt)
    {
        if (tool.Status != "completed" || !IsResourceTool(tool.Name) || receipt.ValueKind == JsonValueKind.Object &&
            receipt.TryGetProperty("isError", out var error) && error.ValueKind == JsonValueKind.True) return null;
        if (tool.Name is "browser.open" or "computer.launch" &&
            (receipt.ValueKind != JsonValueKind.Object || String(receipt, "presentation") != "workspace")) return null;
        var resource = FromReference(receipt);
        if (resource is null && receipt.ValueKind == JsonValueKind.Object && receipt.TryGetProperty("structuredContent", out var structured))
            resource = FromReference(structured);
        return resource is null ? null : resource with
        {
            PresentationPriority = tool.Name is "filesystem.write" or "filesystem.edit"
                ? WorkspacePresentationPriority.CatalogOnly : WorkspacePresentationPriority.Requested
        };
    }

    public static ConversationWorkTab? FromReference(JsonElement reference)
    {
        if (reference.ValueKind != JsonValueKind.Object ||
            reference.TryGetProperty("isError", out var error) && error.ValueKind == JsonValueKind.True) return null;
        string? path = String(reference, "path") ?? String(reference, "relativePath");
        string? title = String(reference, "title") ?? String(reference, "fileName");
        string? version = String(reference, "version") ?? String(reference, "sha256") ?? String(reference, "sourceVersion");
        string? url = String(reference, "url");
        if (Uri.TryCreate(path, UriKind.Absolute, out var pathAddress) && pathAddress.Scheme is "http" or "https")
        {
            // A live browser can still report about:blank while its requested website is loading.
            // 已打开的浏览器仍可能暂报 about:blank；用展示路径定位正在加载的网站，不丢弃真实导航状态。
            if (!Uri.TryCreate(url, UriKind.Absolute, out var currentAddress) || currentAddress.Scheme is not ("http" or "https"))
                url = path;
        }
        else if (!string.IsNullOrWhiteSpace(path)) return File(path, title, version) with
        {
            MountId = Guid.TryParse(String(reference, "mountId"), out Guid mountId) ? mountId : null,
            PresentationPriority = String(reference, "kind") == "task-blob"
                ? WorkspacePresentationPriority.CatalogOnly : WorkspacePresentationPriority.Automatic
        };
        return Uri.TryCreate(url, UriKind.Absolute, out var address) && address.Scheme is "http" or "https"
            ? new("browser/" + address.AbsoluteUri, "browser", Title(title, address.Host), address.AbsoluteUri, version) : null;
    }

    public static ConversationWorkTab File(string path, string? title = null, string? version = null, bool userSelected = false)
    {
        string normalized = path.Replace('/', '\\');
        string name = normalized.Split('\\', StringSplitOptions.RemoveEmptyEntries).LastOrDefault() ?? path;
        string kind = Path.GetExtension(name).ToLowerInvariant() is ".mmd" or ".mermaid" ? "diagram" : "document";
        return new("file/" + normalized.ToUpperInvariant(), kind, Title(title, name), path, version, userSelected);
    }

    public static ConversationWorkTab Terminal(Guid conversationId, string sessionId, string? title = null) =>
        new($"terminal/{conversationId:D}/{sessionId}", "terminal", Title(title, "Terminal"),
            conversationId.ToString("D"), sessionId, PresentationPriority: WorkspacePresentationPriority.Requested);

    public static ConversationWorkTab[] PrioritizeDeliveredOutputs(IReadOnlyList<ConversationWorkTab> resources, string? finalAnswer)
    {
        if (string.IsNullOrEmpty(finalAnswer)) return resources.ToArray();
        var links = DeliveryLinks.Matches(finalAnswer);
        if (links.Count == 0) return resources.ToArray();
        var deliveredKeys = new HashSet<string>(StringComparer.Ordinal);
        var files = resources.Where(tab => tab.Kind is "document" or "diagram" && tab.Target is not null)
            .Select(tab => (Tab: tab, Target: NormalizeFileTarget(tab.Target!))).ToArray();
        foreach (Match link in links)
        {
            string target = NormalizeFileTarget(link.Groups["target"].Value);
            bool relative = !Path.IsPathRooted(target) && !target.Contains(':');
            var matches = files.Where(file => file.Target.Equals(target, StringComparison.OrdinalIgnoreCase) || relative &&
                file.Target.EndsWith("\\" + target, StringComparison.OrdinalIgnoreCase)).Take(2).ToArray();
            // A delivery link can promote an existing receipt, never invent a file or pick an ambiguous basename.
            // 交付链接只提升已有真实回执，不能凭链接捏造文件，也不能猜测重名文件。
            if (matches.Length == 1) deliveredKeys.Add(matches[0].Tab.Key);
        }
        return resources.Select(tab => deliveredKeys.Contains(tab.Key) && tab.PresentationPriority < WorkspacePresentationPriority.Final
            ? tab with { PresentationPriority = WorkspacePresentationPriority.Final } : tab).ToArray();
    }

    private static string NormalizeFileTarget(string value)
    {
        string target = Uri.UnescapeDataString(value.Trim());
        if (Uri.TryCreate(target, UriKind.Absolute, out var address) && address.IsFile) target = address.LocalPath;
        return target.Replace('/', '\\');
    }

    private static string? String(JsonElement value, string key) => value.TryGetProperty(key, out var item) &&
        item.ValueKind == JsonValueKind.String ? item.GetString() : null;
    private static string Title(string? title, string fallback)
    {
        string value = string.IsNullOrWhiteSpace(title) ? fallback : title;
        return value.Length <= 256 ? value : value[..256];
    }
}
