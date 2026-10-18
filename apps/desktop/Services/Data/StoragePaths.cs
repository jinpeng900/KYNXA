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
    // Clipboard drafts are import sources, so they must stay outside protected application data.
    // 剪贴板草稿是待导入的源文件，必须暂存在应用私有数据保护范围之外。
    public static string AttachmentDraftDirectory => Path.Combine(Path.GetTempPath(), "KYNXA", "AttachmentDrafts");
    public static void Reload() => DesktopDirectory = ResolveDesktopDirectory();

    private static string ResolveDesktopDirectory()
    {
        // Initialize WinRT application storage before the first page's Loaded event,
        // including when the data files themselves live in a custom directory.
        // 首个页面 Loaded 事件前初始化 WinRT 应用存储，即使实际数据位于自定义目录也一样。
        try
        {
            _ = Windows.ApplicationModel.Package.Current.Id;
            _ = ApplicationData.Current.LocalFolder.Path;
        }
        catch (InvalidOperationException)
        {
            // A self-contained installer has no package identity; its explicit data root remains authoritative.
            // 自包含安装无需包身份；安装器配置的数据位置仍为唯一正式目录。
        }
        DataRoot = ApplicationPaths.DataRoot;
        string path = ApplicationPaths.LegacyDesktopDirectory ?? Path.Combine(DataRoot, "Desktop");
        Directory.CreateDirectory(path);
        return path;
    }
}
