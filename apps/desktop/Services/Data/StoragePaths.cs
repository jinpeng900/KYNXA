using Windows.Storage;

namespace KYNXA_Desktop.Services;

/// <summary>
/// This installation owns its default data and path pointer; users may select another location.
/// 当前安装目录拥有默认数据与路径指针，用户仍可选择其他存储位置。
/// </summary>
public static class StoragePaths
{
    public static string DefaultRoot => Path.Combine(ApplicationPaths.Root, "Data");
    public static string PointerPath => Path.Combine(ApplicationPaths.Root, "storage.json");
    public static string MigrationLockPath => Path.Combine(Path.GetDirectoryName(PointerPath)!, "storage-migration.lock");
    public static bool IsMigrating { get; set; }
    public static bool EnvironmentControlled => !string.IsNullOrWhiteSpace(Environment.GetEnvironmentVariable("KYNXA_DATA_HOME"))
        || !string.IsNullOrWhiteSpace(Environment.GetEnvironmentVariable("KYNXA_MODEL_HOME"));
    public static string? DataRoot { get; private set; }
    public static string DesktopDirectory { get; private set; } = ResolveDesktopDirectory();
    public static void Reload() => DesktopDirectory = ResolveDesktopDirectory();

    private static string ResolveDesktopDirectory()
    {
        // Initialize WinRT application storage before the first page's Loaded event,
        // including when the data files themselves live in a custom directory.
        // 首个页面 Loaded 事件前初始化 WinRT 应用存储，即使实际数据位于自定义目录也一样。
        _ = ApplicationData.Current.LocalFolder.Path;
        DataRoot = ApplicationPaths.DataRoot;
        string path = Path.Combine(DataRoot, "Desktop");
        Directory.CreateDirectory(path);
        return path;
    }
}
