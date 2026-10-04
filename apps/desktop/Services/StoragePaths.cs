using System.Text.Json;
using Windows.Storage;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Shared data-root pointer; legacy locations remain the default until migrated.
/// 共享数据根目录指针；迁移前继续使用旧版默认位置。
/// </summary>
public static class StoragePaths
{
    public static string DefaultRoot => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "KYNXA", "Data");
    public static string PointerPath => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".kynxa", "storage.json");
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
        string legacyDirectory = ApplicationData.Current.LocalFolder.Path;
        string? root = Environment.GetEnvironmentVariable("KYNXA_DATA_HOME");
        string pointer = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
            ".kynxa", "storage.json");
        if (string.IsNullOrWhiteSpace(root) && File.Exists(pointer))
        {
            using var document = JsonDocument.Parse(File.ReadAllText(pointer));
            root = document.RootElement.GetProperty("dataRoot").GetString();
            if (string.IsNullOrWhiteSpace(root)) throw new InvalidDataException("KYNXA 存储目录配置为空。");
        }
        if (!string.IsNullOrWhiteSpace(root) && !Path.IsPathFullyQualified(root))
            throw new InvalidDataException("KYNXA 存储目录必须为绝对路径。");
        if (string.IsNullOrWhiteSpace(root) && !Directory.EnumerateFiles(legacyDirectory, "*.json").Any()
            && !Directory.Exists(Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".kynxa", "models")))
        {
            root = DefaultRoot;
            Directory.CreateDirectory(Path.GetDirectoryName(PointerPath)!);
            File.WriteAllText(PointerPath + ".tmp", JsonSerializer.Serialize(new { version = 1, dataRoot = root }));
            File.Move(PointerPath + ".tmp", PointerPath, overwrite: true);
        }
        DataRoot = root;
        string path = string.IsNullOrWhiteSpace(root) ? legacyDirectory
            : Path.Combine(Path.GetFullPath(root), "Desktop");
        Directory.CreateDirectory(path);
        return path;
    }
}
