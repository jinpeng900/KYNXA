using System.Text.Json;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Resolves portable paths from this installation, independent of the working directory.
/// 按当前安装位置解析便携路径，不依赖启动工作目录或用户目录。
/// </summary>
public static class ApplicationPaths
{
    private static string? _packagedStateRoot;
    private static string? _packagedPointerDirectory;
    private static string? _legacyModelHome;
    private static string? _legacyDesktopDirectory;

    public static string Root => Path.TrimEndingDirectorySeparator(Path.GetFullPath(
        Environment.GetEnvironmentVariable("KYNXA_APPLICATION_ROOT") is string configured && !string.IsNullOrWhiteSpace(configured)
            ? configured : AppContext.BaseDirectory));

    public static string StateRoot => _packagedStateRoot ?? Root;
    public static bool UsesPackagedStorage => _packagedStateRoot is not null;
    public static string PointerDirectory => _packagedPointerDirectory ?? Root;
    public static string DefaultDataRoot => Path.Combine(StateRoot, "Data");
    public static string DefaultExtensionRoot => Path.Combine(StateRoot, "Extensions");
    public static string StoragePointerPath => Environment.GetEnvironmentVariable("KYNXA_STORAGE_POINTER") is string pointer && !string.IsNullOrWhiteSpace(pointer)
        ? Resolve(pointer) : Path.Combine(PointerDirectory, "storage.json");
    public static string MigrationLockPath => Environment.GetEnvironmentVariable("KYNXA_STORAGE_MIGRATION_LOCK") is string marker && !string.IsNullOrWhiteSpace(marker)
        ? Resolve(marker) : Path.Combine(Path.GetDirectoryName(StoragePointerPath)!, "storage-migration.lock");
    public static string DefaultModelHome => _legacyModelHome ?? Path.Combine(DefaultDataRoot, "Models");
    public static string DataRoot => ResolveDataRoot(Root, Environment.GetEnvironmentVariable("KYNXA_DATA_HOME"),
        Environment.GetEnvironmentVariable("KYNXA_MODEL_HOME"), StoragePointerPath, DefaultDataRoot, _legacyModelHome);
    public static string ModelHome => Environment.GetEnvironmentVariable("KYNXA_MODEL_HOME") is string models && !string.IsNullOrWhiteSpace(models)
        ? Resolve(models) : UseLegacyModelHome ? _legacyModelHome! : Path.Combine(DataRoot, "Models");
    public static string? LegacyDesktopDirectory => UseLegacyModelHome ? _legacyDesktopDirectory : null;
    public static string? LegacyExtensionRoot
    {
        get
        {
            string data = DataRoot;
            // Agent alone may contain a new task ledger; it does not identify legacy extensions.
            // Agent 目录也可能只有新任务账本，不能据此判定旧扩展位置。
            return UsesPackagedStorage && (Directory.Exists(Path.Combine(data, "MCP")) || Directory.Exists(Path.Combine(data, "Skills")))
                ? data : null;
        }
    }
    private static bool UseLegacyModelHome => _legacyModelHome is not null
        && string.IsNullOrWhiteSpace(Environment.GetEnvironmentVariable("KYNXA_MODEL_HOME"))
        && string.IsNullOrWhiteSpace(Environment.GetEnvironmentVariable("KYNXA_DATA_HOME"))
        && !File.Exists(StoragePointerPath);
    public static string WebViewDirectory => Environment.GetEnvironmentVariable("WEBVIEW2_USER_DATA_FOLDER") is string configured && !string.IsNullOrWhiteSpace(configured)
        ? Resolve(configured) : Path.Combine(DataRoot, "Desktop", "WebView2");

    // Packaged payloads are replaceable/read-only; preserve the established writable roots.
    // 包载荷可能只读且会被更新替换；继续使用既有的可写数据与指针位置。
    public static void ConfigurePackagedStorage(string stateRoot, string pointerDirectory,
        string? legacyModelHome = null, string? legacyDesktopDirectory = null)
    {
        _packagedStateRoot = Resolve(stateRoot);
        _packagedPointerDirectory = Resolve(pointerDirectory);
        _legacyModelHome = legacyModelHome is null ? null : Resolve(legacyModelHome);
        _legacyDesktopDirectory = legacyDesktopDirectory is null ? null : Resolve(legacyDesktopDirectory);
    }

    public static string ResolveDataRoot(string applicationRoot, string? environmentRoot = null, string? modelHome = null,
        string? pointerPath = null, string? defaultDataRoot = null, string? defaultModelHome = null)
    {
        if (!string.IsNullOrWhiteSpace(modelHome))
        {
            string models = Resolve(modelHome, applicationRoot);
            return string.Equals(Path.GetFileName(models), "Models", StringComparison.OrdinalIgnoreCase)
                ? Path.GetDirectoryName(models)! : Path.Combine(models, "Conversations");
        }
        if (!string.IsNullOrWhiteSpace(environmentRoot)) return Resolve(environmentRoot, applicationRoot);
        string pointer = pointerPath ?? Path.Combine(applicationRoot, "storage.json");
        if (!File.Exists(pointer)) return defaultModelHome is null
            ? defaultDataRoot ?? Path.Combine(applicationRoot, "Data")
            : ResolveDataRoot(applicationRoot, modelHome: defaultModelHome);
        using var document = JsonDocument.Parse(File.ReadAllText(pointer));
        if (!document.RootElement.TryGetProperty("version", out var version) || !version.TryGetInt32(out int number) || number != 1)
            throw new InvalidDataException("KYNXA 存储目录配置版本无效。");
        string configuredRoot = document.RootElement.GetProperty("dataRoot").GetString()
            ?? throw new InvalidDataException("KYNXA 存储目录配置为空。");
        return Resolve(configuredRoot, Path.GetDirectoryName(pointer));
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
