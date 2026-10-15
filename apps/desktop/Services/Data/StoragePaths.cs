using Windows.Storage;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Portable installations own their state; packaged installations retain persistent user state.
/// 便携安装拥有本目录状态；打包安装继续使用用户持久数据位置。
/// </summary>
public static class StoragePaths
{
    public static string DefaultRoot => ApplicationPaths.DefaultDataRoot;
    public static string PointerPath => ApplicationPaths.StoragePointerPath;
    public static string MigrationLockPath => ApplicationPaths.MigrationLockPath;
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
        string path = ApplicationPaths.LegacyDesktopDirectory ?? Path.Combine(DataRoot, "Desktop");
        Directory.CreateDirectory(path);
        return path;
    }
}
