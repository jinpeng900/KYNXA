using System.Text.Json;
using Windows.Storage;

namespace KYNXA_Desktop.Services;

/// <summary>Shared data-root pointer; legacy locations remain the default until migrated.</summary>
public static class StoragePaths
{
    public static string DesktopDirectory { get; } = ResolveDesktopDirectory();

    private static string ResolveDesktopDirectory()
    {
        // Initialize WinRT application storage before the first page's Loaded event,
        // including when the data files themselves live in a custom directory.
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
        string path = string.IsNullOrWhiteSpace(root) ? legacyDirectory
            : Path.Combine(Path.GetFullPath(root), "Desktop");
        Directory.CreateDirectory(path);
        return path;
    }
}
