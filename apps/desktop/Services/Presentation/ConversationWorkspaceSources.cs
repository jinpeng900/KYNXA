using System.Text.Json;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Written outputs and explicit display actions create tabs; ordinary source reads stay in the task context.
/// 写入产物与明确展示动作生成标签；普通源码读取保留在任务上下文，不堆满文件栏。
/// </summary>
public static class ConversationWorkspaceSources
{
    public static bool IsResourceTool(string? name) => name is "artifact.open" or "filesystem.write" or "filesystem.edit"
        or "browser.open" or "computer.launch";

    public static ConversationWorkTab? FromTool(ToolActivity tool, JsonElement receipt)
    {
        if (tool.Status != "completed" || !IsResourceTool(tool.Name) || receipt.ValueKind == JsonValueKind.Object &&
            receipt.TryGetProperty("isError", out var error) && error.ValueKind == JsonValueKind.True) return null;
        if (tool.Name is "browser.open" or "computer.launch" &&
            (receipt.ValueKind != JsonValueKind.Object || String(receipt, "presentation") != "workspace")) return null;
        var resource = FromReference(receipt);
        if (resource is not null) return resource;
        if (receipt.ValueKind == JsonValueKind.Object && receipt.TryGetProperty("structuredContent", out var structured))
            return FromReference(structured);
        return null;
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
            url ??= path;
        else if (!string.IsNullOrWhiteSpace(path)) return File(path, title, version);
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

    private static string? String(JsonElement value, string key) => value.TryGetProperty(key, out var item) &&
        item.ValueKind == JsonValueKind.String ? item.GetString() : null;
    private static string Title(string? title, string fallback)
    {
        string value = string.IsNullOrWhiteSpace(title) ? fallback : title;
        return value.Length <= 256 ? value : value[..256];
    }
}
