using System.Text.Json;

namespace KYNXA_Desktop.Services;

public sealed record ExtensionPathResolution(string Root, bool UsesLegacyRoot);

/// <summary>Extension locations are independent of WinRT and conversation storage initialization.</summary>
public static class ExtensionPaths
{
    internal static readonly string[] LayoutDirectories = ["Agent", "Skills", "MCP", "MCP/npm-cache", "MCP/browser-cache",
        "MCP/uv-cache", "MCP/uv-tools", "MCP/bin", "MCP/python", "MCP/python-bin", "MCP/runtimes",
        "Backups", "Backups/Extensions", "Backups/Extensions/Migrations"];
    public static string DefaultRoot => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "KYNXA", "Extensions");
    public static string? LegacyRoot { get; set; }
    public static string Root { get; private set; } = DefaultRoot;
    public static bool UsesLegacyRoot { get; private set; }
    public static string PointerPath => Environment.GetEnvironmentVariable("KYNXA_EXTENSION_POINTER") is string pointer && !string.IsNullOrWhiteSpace(pointer)
        ? NormalizePointer(pointer)
        : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".kynxa", "extensions.json");
    public static bool EnvironmentControlled => new[] { "KYNXA_EXTENSION_HOME", "KYNXA_DATA_HOME", "KYNXA_MODEL_HOME" }
        .Any(name => !string.IsNullOrWhiteSpace(Environment.GetEnvironmentVariable(name)));

    public static void Reload()
    {
        string? environmentRoot = Environment.GetEnvironmentVariable("KYNXA_EXTENSION_HOME");
        bool explicitPointer = !string.IsNullOrWhiteSpace(Environment.GetEnvironmentVariable("KYNXA_EXTENSION_POINTER"));
        bool isolatedData = new[] { "KYNXA_DATA_HOME", "KYNXA_MODEL_HOME" }
            .Any(name => !string.IsNullOrWhiteSpace(Environment.GetEnvironmentVariable(name)));
        var resolved = Resolve(LegacyRoot ?? DefaultRoot, environmentRoot, PointerPath, ignorePointer: isolatedData && !explicitPointer);
        Root = resolved.Root;
        UsesLegacyRoot = resolved.UsesLegacyRoot;
    }

    // All inputs are explicit so tests never initialize StoragePaths or inspect a user's pointer.
    public static ExtensionPathResolution Resolve(string legacyRoot, string? environmentRoot, string pointerPath, bool ignorePointer = false)
    {
        if (!string.IsNullOrWhiteSpace(environmentRoot)) return new(NormalizeRoot(environmentRoot), false);
        if (!ignorePointer)
        {
            string pointer = NormalizePointer(pointerPath);
            StorageMigrationService.RejectLinks(pointer);
            if (Directory.Exists(pointer)) throw InvalidPointer();
            if (File.Exists(pointer))
            {
                if (new FileInfo(pointer).Length > 16 * 1024) throw InvalidPointer();
                return new(ParsePointer(File.ReadAllBytes(pointer)), false);
            }
        }
        return new(NormalizeRoot(legacyRoot), true);
    }

    internal static string ParsePointer(byte[] bytes)
    {
        if (bytes.Length > 16 * 1024) throw InvalidPointer();
        try
        {
            ReadOnlyMemory<byte> content = bytes;
            if (content.Span.StartsWith(new byte[] { 0xEF, 0xBB, 0xBF })) content = content[3..];
            using var document = JsonDocument.Parse(content);
            var value = document.RootElement;
            if (value.ValueKind != JsonValueKind.Object) throw InvalidPointer();
            var names = new HashSet<string>(StringComparer.Ordinal);
            foreach (var property in value.EnumerateObject())
                if (!names.Add(property.Name)) throw InvalidPointer();
            if (!value.TryGetProperty("version", out var version) || version.ValueKind != JsonValueKind.Number || !version.TryGetInt32(out int number)) throw InvalidPointer();
            if (number != 1) throw new InvalidDataException(UiText.Get("扩展配置版本不受当前程序支持，原文件已保留。"));
            if (!value.TryGetProperty("extensionRoot", out var root) || root.ValueKind != JsonValueKind.String) throw InvalidPointer();
            return NormalizeRoot(root.GetString()!);
        }
        catch (JsonException error) { throw new InvalidDataException(UiText.Get("扩展配置格式无效，原文件已保留。"), error); }
    }

    internal static string NormalizeRoot(string path)
    {
        if (string.IsNullOrWhiteSpace(path) || !Path.IsPathFullyQualified(path) || path.StartsWith(@"\\", StringComparison.Ordinal)
            || path.IndexOfAny(['\0', '\r', '\n']) >= 0)
            throw new InvalidOperationException(UiText.Get("请选择本机磁盘上的绝对路径。"));
        string root = Path.TrimEndingDirectorySeparator(Path.GetFullPath(path));
        if (string.Equals(root, Path.GetPathRoot(root), StringComparison.OrdinalIgnoreCase))
            throw new InvalidOperationException(UiText.Get("不能直接使用磁盘根目录，请选择一个空文件夹。"));
        StorageMigrationService.RejectLinks(root);
        if (File.Exists(root)) throw InvalidPointer();
        return root;
    }

    internal static bool ValidateLayout(string root)
    {
        root = NormalizeRoot(root);
        foreach (string name in LayoutDirectories)
        {
            string path = Path.Combine(root, name);
            StorageMigrationService.RejectLinks(path);
            if (File.Exists(path)) throw InvalidPointer();
        }
        foreach (string name in new[] { "Agent/config.json", "extension-layout.json", "extension-migration-info.json", "extensions-pointer.previous.json" })
        {
            string path = Path.Combine(root, name);
            StorageMigrationService.RejectLinks(path);
            if (Directory.Exists(path)) throw InvalidPointer();
        }
        string metadata = Path.Combine(root, "extension-layout.json");
        if (!File.Exists(metadata)) return false;
        if (new FileInfo(metadata).Length > 16 * 1024) throw InvalidPointer();
        byte[] bytes = File.ReadAllBytes(metadata);
        if (bytes.Length > 16 * 1024) throw InvalidPointer();
        try
        {
            ReadOnlyMemory<byte> content = bytes;
            if (content.Span.StartsWith(new byte[] { 0xEF, 0xBB, 0xBF })) content = content[3..];
            using var document = JsonDocument.Parse(content);
            var value = document.RootElement;
            if (value.ValueKind != JsonValueKind.Object) throw InvalidPointer();
            var names = new HashSet<string>(StringComparer.Ordinal);
            foreach (var property in value.EnumerateObject()) if (!names.Add(property.Name)) throw InvalidPointer();
            if (!value.TryGetProperty("version", out var version) || version.ValueKind != JsonValueKind.Number || !version.TryGetInt32(out int number) || number != 1)
                throw new InvalidDataException(UiText.Get("扩展配置版本不受当前程序支持，原文件已保留。"));
        }
        catch (JsonException error) { throw new InvalidDataException(UiText.Get("扩展配置格式无效，原文件已保留。"), error); }
        return true;
    }

    // The gateway owns normal startup initialization. Native settings use this only
    // for an inactive, verified migration target while holding the maintenance marker.
    internal static void EnsureLayout(string root)
    {
        bool existing = ValidateLayout(root);
        Directory.CreateDirectory(root);
        foreach (string name in LayoutDirectories)
        {
            string path = Path.Combine(root, name);
            StorageMigrationService.RejectLinks(path);
            Directory.CreateDirectory(path);
            StorageMigrationService.RejectLinks(path);
        }
        if (!existing)
        {
            string path = Path.Combine(root, "extension-layout.json");
            StorageMigrationService.RejectLinks(path);
            string temporary = Path.Combine(root, ".extension-layout." + Guid.NewGuid().ToString("N") + ".tmp");
            try
            {
                using (var stream = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None))
                {
                    stream.Write("{\"version\":1}"u8);
                    stream.Flush(flushToDisk: true);
                }
                StorageMigrationService.RejectLinks(path);
                try { File.Move(temporary, path, overwrite: false); }
                catch (IOException) when (File.Exists(path)) { ValidateLayout(root); }
            }
            finally { if (File.Exists(temporary)) File.Delete(temporary); }
        }
        ValidateLayout(root);
    }

    internal static string NormalizePointer(string path)
    {
        if (!Path.IsPathFullyQualified(path) || path.StartsWith(@"\\", StringComparison.Ordinal)
            || path.IndexOfAny(['\0', '\r', '\n']) >= 0 || Path.EndsInDirectorySeparator(path))
            throw new InvalidOperationException(UiText.Get("请选择本机磁盘上的绝对路径。"));
        string pointer = Path.GetFullPath(path);
        StorageMigrationService.RejectLinks(pointer);
        return pointer;
    }

    private static InvalidDataException InvalidPointer() => new(UiText.Get("扩展配置格式无效，原文件已保留。"));
}
