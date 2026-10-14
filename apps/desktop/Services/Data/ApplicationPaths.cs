using System.Text.Json;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Resolves portable paths from this installation, independent of the working directory.
/// 按当前安装位置解析便携路径，不依赖启动工作目录或用户目录。
/// </summary>
public static class ApplicationPaths
{
    public static string Root => Path.TrimEndingDirectorySeparator(Path.GetFullPath(
        Environment.GetEnvironmentVariable("KYNXA_APPLICATION_ROOT") is string configured && !string.IsNullOrWhiteSpace(configured)
            ? configured : AppContext.BaseDirectory));

    public static string DataRoot => ResolveDataRoot(Root, Environment.GetEnvironmentVariable("KYNXA_DATA_HOME"),
        Environment.GetEnvironmentVariable("KYNXA_MODEL_HOME"));
    public static string WebViewDirectory => Environment.GetEnvironmentVariable("WEBVIEW2_USER_DATA_FOLDER") is string configured && !string.IsNullOrWhiteSpace(configured)
        ? Resolve(configured) : Path.Combine(DataRoot, "Desktop", "WebView2");

    public static string ResolveDataRoot(string applicationRoot, string? environmentRoot = null, string? modelHome = null)
    {
        if (!string.IsNullOrWhiteSpace(modelHome))
        {
            string models = Resolve(modelHome, applicationRoot);
            return string.Equals(Path.GetFileName(models), "Models", StringComparison.OrdinalIgnoreCase)
                ? Path.GetDirectoryName(models)! : Path.Combine(models, "Conversations");
        }
        if (!string.IsNullOrWhiteSpace(environmentRoot)) return Resolve(environmentRoot, applicationRoot);
        string pointer = Path.Combine(applicationRoot, "storage.json");
        if (!File.Exists(pointer)) return Path.Combine(applicationRoot, "Data");
        using var document = JsonDocument.Parse(File.ReadAllText(pointer));
        if (!document.RootElement.TryGetProperty("version", out var version) || !version.TryGetInt32(out int number) || number != 1)
            throw new InvalidDataException("KYNXA 存储目录配置版本无效。");
        string configuredRoot = document.RootElement.GetProperty("dataRoot").GetString()
            ?? throw new InvalidDataException("KYNXA 存储目录配置为空。");
        return Resolve(configuredRoot, applicationRoot);
    }

    public static string Resolve(string path, string? applicationRoot = null)
    {
        if (string.IsNullOrWhiteSpace(path)) throw new InvalidDataException("KYNXA 存储目录配置为空。");
        return Path.TrimEndingDirectorySeparator(Path.GetFullPath(path, applicationRoot ?? Root));
    }

    // Keep installation-owned selections relative when the entire folder is moved.
    // 安装目录内部的自选位置保存为相对路径，整个文件夹搬移后继续适配。
    public static string Store(string path, string? applicationRoot = null)
    {
        string root = applicationRoot ?? Root;
        string resolved = Resolve(path, root);
        string relative = Path.GetRelativePath(root, resolved);
        return relative == ".." || relative.StartsWith(".." + Path.DirectorySeparatorChar, StringComparison.Ordinal)
            || Path.IsPathFullyQualified(relative) ? resolved : relative;
    }
}
