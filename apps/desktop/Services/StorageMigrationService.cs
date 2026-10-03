using System.Security.Cryptography;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace KYNXA_Desktop.Services;

public sealed record StorageMigrationResult(string DataRoot, int VerifiedFiles);

/// <summary>Copy and verify while all application writers are paused; activate the pointer last.</summary>
public static class StorageMigrationService
{
    private static readonly string[] ConversationEntries =
        ["Projects", "Chats", "Trash", "Backups", "Memory", "Index", "Agent", "Skills", "MCP", "extension-layout.json", "extension-migration-info.json", "extensions-pointer.previous.json", "settings.json", "catalog.json", ".conversations-v1.json", ".catalog-transaction.json"];

    internal sealed record CopyRoot(string Source, string Name, string[]? Include = null, string[]? Exclude = null);

    public static bool IsWithin(string path, string parent) =>
        string.Equals(Path.TrimEndingDirectorySeparator(Path.GetFullPath(path)), Path.TrimEndingDirectorySeparator(Path.GetFullPath(parent)), StringComparison.OrdinalIgnoreCase)
        || Path.GetFullPath(path).StartsWith(Path.TrimEndingDirectorySeparator(Path.GetFullPath(parent)) + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase);

    public static async Task<StorageMigrationResult> MoveAsync(string desktop, string models, string target,
        string pointer, IProgress<string>? progress = null, CancellationToken cancellationToken = default,
        Func<string, CancellationToken, Task>? initializeTarget = null, bool migrateExtensions = true)
    {
        if (!Path.IsPathFullyQualified(target) || target.StartsWith(@"\\"))
            throw new InvalidOperationException(UiText.Get("请选择本机磁盘上的绝对路径。"));
        target = Path.TrimEndingDirectorySeparator(Path.GetFullPath(target));
        if (target == Path.GetPathRoot(target)) throw new InvalidOperationException(UiText.Get("不能直接使用磁盘根目录，请选择一个空文件夹。"));
        models = Path.TrimEndingDirectorySeparator(Path.GetFullPath(models));
        desktop = Path.TrimEndingDirectorySeparator(Path.GetFullPath(desktop));
        bool standardModelsDirectory = string.Equals(Path.GetFileName(models), "Models", StringComparison.OrdinalIgnoreCase);
        string conversationRoot = standardModelsDirectory ? Path.GetDirectoryName(models)! : Path.Combine(models, "Conversations");
        foreach (var source in new[] { desktop, models }.Concat(ConversationEntries.Select(name => Path.Combine(conversationRoot, name))))
            if (IsWithin(target, source) || IsWithin(source, target))
                throw new InvalidOperationException(UiText.Get("新目录不能与原数据目录相同、包含原目录或位于原目录内部。"));
        RejectLinks(target);
        if (Directory.Exists(target) && Directory.EnumerateFileSystemEntries(target).Any())
            throw new InvalidOperationException(UiText.Get("目标文件夹不是空的，请选择空文件夹，避免覆盖已有数据。"));
        await ValidateLayoutSettingsAsync(conversationRoot, cancellationToken);
        if (migrateExtensions)
        {
            ExtensionPaths.ValidateLayout(conversationRoot);
            await ExtensionConfigurationMigration.ValidateAsync(conversationRoot, cancellationToken);
        }
        byte[]? oldPointer = File.Exists(pointer) ? await File.ReadAllBytesAsync(pointer, cancellationToken) : null;
        // A custom model home keeps conversations below itself; move that subtree to
        // the new Data root exactly once, where the standard Models layout expects it.
        var roots = new[] {
            new CopyRoot(desktop, "Desktop"),
            new CopyRoot(models, "Models", Exclude: standardModelsDirectory ? null : ["Conversations"]),
            new CopyRoot(conversationRoot, "", Include: migrateExtensions ? ConversationEntries : ConversationEntries.Except(["Agent", "Skills", "MCP", "extension-layout.json", "extension-migration-info.json", "extensions-pointer.previous.json"]).ToArray())
        };
        var snapshots = roots.Select(root => (Root: root, Existed: Directory.Exists(root.Source), Files: ListFiles(root), Directories: ListFiles(root, true))).ToArray();
        Directory.CreateDirectory(target);
        var verified = new List<(string Source, string Destination, byte[] Hash)>();
        foreach (var root in snapshots)
        {
            Directory.CreateDirectory(Path.Combine(target, root.Root.Name));
            foreach (var directory in root.Directories)
                Directory.CreateDirectory(Path.Combine(target, root.Root.Name, directory));
            foreach (var relative in root.Files)
            {
                cancellationToken.ThrowIfCancellationRequested();
                progress?.Report(string.Format(UiText.Get("正在复制并校验文件（{0}）…"), verified.Count + 1));
                string source = Path.Combine(root.Root.Source, relative), destination = Path.Combine(target, root.Root.Name, relative);
                RejectLinks(source);
                byte[] digest = await HashAsync(source, cancellationToken);
                Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
                await using (var input = new FileStream(source, FileMode.Open, FileAccess.Read, FileShare.Read))
                await using (var output = new FileStream(destination, FileMode.CreateNew, FileAccess.Write, FileShare.None))
                    await input.CopyToAsync(output, cancellationToken);
                byte[] copiedDigest = await HashAsync(destination, cancellationToken);
                if (!digest.SequenceEqual(copiedDigest)) throw new IOException(UiText.Get("文件复制校验失败，原数据未改动。"));
                verified.Add((source, destination, digest));
            }
        }
        progress?.Report(UiText.Get("正在核对数据并更新内置项目路径…"));
        await VerifySourceAsync();
        foreach (var root in snapshots)
            if (!root.Directories.SequenceEqual(ListFiles(new(Path.Combine(target, root.Root.Name), "", root.Root.Include, root.Root.Exclude), true)))
                throw new IOException(UiText.Get("文件复制校验失败，原数据未改动。"));
        foreach (var file in verified)
        {
            byte[] currentDigest = await HashAsync(file.Destination, cancellationToken);
            if (!file.Hash.SequenceEqual(currentDigest)) throw new IOException(UiText.Get("文件复制校验失败，原数据未改动。"));
        }

        string projectsPath = Path.Combine(target, "Desktop", "projects.json");
        if (File.Exists(projectsPath))
        {
            var projects = JsonNode.Parse(await File.ReadAllTextAsync(projectsPath, cancellationToken))?.AsArray()
                ?? throw new InvalidDataException(UiText.Get("项目数据无效。"));
            foreach (var project in projects)
                if (project?["FolderPath"]?.GetValue<string>() is string folder && IsWithin(folder, Path.Combine(desktop, "Projects")))
                    project["FolderPath"] = Path.Combine(target, "Desktop", Path.GetRelativePath(desktop, folder));
            await File.WriteAllTextAsync(projectsPath, projects.ToJsonString(new JsonSerializerOptions { WriteIndented = true }), cancellationToken);
        }
        foreach (string metadata in new[] { "catalog.json", ".catalog-transaction.json" })
        {
            string path = Path.Combine(target, metadata);
            if (!File.Exists(path)) continue;
            var document = JsonNode.Parse(await File.ReadAllTextAsync(path, cancellationToken))
                ?? throw new InvalidDataException(UiText.Get("会话目录数据无效。"));
            RelocateProjectFolders(document, desktop, target);
            await File.WriteAllTextAsync(path, document.ToJsonString(new JsonSerializerOptions { WriteIndented = true }), cancellationToken);
        }
        // These manifests are derived from the catalog. Keep the copied view in
        // sync until the gateway next rebuilds it from the canonical metadata.
        string projectDirectory = Path.Combine(target, "Projects");
        if (Directory.Exists(projectDirectory))
            foreach (string directory in Directory.EnumerateDirectories(projectDirectory))
            {
                string path = Path.Combine(directory, "project.json");
                if (!File.Exists(path)) continue;
                var document = JsonNode.Parse(await File.ReadAllTextAsync(path, cancellationToken))
                    ?? throw new InvalidDataException(UiText.Get("项目清单数据无效。"));
                RelocateProjectFolders(document, desktop, target);
                await File.WriteAllTextAsync(path, document.ToJsonString(new JsonSerializerOptions { WriteIndented = true }), cancellationToken);
            }
        string localServerPath = Path.Combine(target, "Models", "local-server.json");
        if (File.Exists(localServerPath))
        {
            var local = JsonNode.Parse(await File.ReadAllTextAsync(localServerPath, cancellationToken))?.AsObject()
                ?? throw new InvalidDataException(UiText.Get("本地模型配置无效。"));
            foreach (string key in new[] { "modelPath", "serverPath", "backendPath" })
                if (local[key]?.GetValue<string>() is string path)
                    foreach (var root in roots.Where(root => root.Name.Length > 0))
                        if (IsWithin(path, root.Source)) local[key] = Path.Combine(target, root.Name, Path.GetRelativePath(root.Source, path));
            await File.WriteAllTextAsync(localServerPath, local.ToJsonString(new JsonSerializerOptions { WriteIndented = true }), cancellationToken);
        }
        // Refuse an incompatible/corrupt destination before making it active.
        if (migrateExtensions)
        {
            await ExtensionConfigurationMigration.RelocateAgentConfigAsync(conversationRoot, target, cancellationToken);
            ExtensionPaths.EnsureLayout(target);
        }
        await ValidateLayoutSettingsAsync(target, cancellationToken);
        // The offline canonical initializer may rebuild projections and append recovery
        // events. Credentials, extension packages, attachments and backups remain exact.
        var preservedFiles = new List<(string Path, byte[] Hash)>();
        foreach (var file in verified)
        {
            string relative = Path.GetRelativePath(target, file.Destination).Replace(Path.DirectorySeparatorChar, '/');
            bool initializerOwned = relative is "catalog.json" or ".catalog-transaction.json" or ".conversations-v1.json" or "settings.json"
                || relative.StartsWith("Index/", StringComparison.OrdinalIgnoreCase)
                || relative.EndsWith("/project.json", StringComparison.OrdinalIgnoreCase)
                || relative.EndsWith("/events.jsonl", StringComparison.OrdinalIgnoreCase)
                || relative.EndsWith("/context.json", StringComparison.OrdinalIgnoreCase);
            if (!initializerOwned) preservedFiles.Add((file.Destination, await HashAsync(file.Destination, cancellationToken)));
        }
        if (initializeTarget is not null)
        {
            progress?.Report(UiText.Get("正在初始化新目录并检查会话记录…"));
            await initializeTarget(target, cancellationToken);
            await ValidateLayoutSettingsAsync(target, cancellationToken);
        }
        await VerifySourceAsync();
        foreach (var file in preservedFiles)
        {
            byte[] digest = await HashAsync(file.Path, cancellationToken);
            if (!file.Hash.SequenceEqual(digest))
                throw new IOException(UiText.Get("文件复制校验失败，原数据未改动。"));
        }
        // Walk all destination entries again so a late link or occupied directory cannot activate.
        ListFiles(new(target, ""));
        if (migrateExtensions) ExtensionPaths.ValidateLayout(target);
        cancellationToken.ThrowIfCancellationRequested();
        byte[]? currentPointer = File.Exists(pointer) ? await File.ReadAllBytesAsync(pointer, cancellationToken) : null;
        if (!(oldPointer ?? []).SequenceEqual(currentPointer ?? [])) throw new IOException(UiText.Get("存储配置被其他实例修改，请重新打开设置。"));
        if (oldPointer is not null) await File.WriteAllBytesAsync(Path.Combine(target, "storage-pointer.previous.json"), oldPointer, cancellationToken);
        await File.WriteAllTextAsync(Path.Combine(target, "migration-info.json"), JsonSerializer.Serialize(new
        { version = 1, completedAt = DateTimeOffset.UtcNow, verifiedFiles = verified.Count, originalFilesRetained = true }), cancellationToken);
        Directory.CreateDirectory(Path.GetDirectoryName(pointer)!);
        string temporary = pointer + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            await File.WriteAllTextAsync(temporary, JsonSerializer.Serialize(new { version = 1, dataRoot = target }), cancellationToken);
            cancellationToken.ThrowIfCancellationRequested();
            File.Move(temporary, pointer, overwrite: true);
        }
        finally { if (File.Exists(temporary)) File.Delete(temporary); }
        return new StorageMigrationResult(target, verified.Count);

        async Task VerifySourceAsync()
        {
            foreach (var snapshot in snapshots)
                if (snapshot.Existed != Directory.Exists(snapshot.Root.Source)
                    || !snapshot.Files.SequenceEqual(ListFiles(snapshot.Root))
                    || !snapshot.Directories.SequenceEqual(ListFiles(snapshot.Root, true)))
                    throw new IOException(UiText.Get("迁移期间原目录发生变化，请停止其他 KYNXA 实例后重试。"));
            foreach (var file in verified)
            {
                byte[] digest = await HashAsync(file.Source, cancellationToken);
                if (!file.Hash.SequenceEqual(digest))
                    throw new IOException(UiText.Get("迁移期间文件被修改，尚未切换存储位置。"));
            }
        }
    }

    private static async Task ValidateLayoutSettingsAsync(string root, CancellationToken cancellationToken)
    {
        string path = Path.Combine(root, "settings.json");
        if (!File.Exists(path))
        {
            if (Directory.Exists(path)) throw new InvalidDataException(UiText.Get("存储设置文件路径被文件夹占用。"));
            return;
        }
        RejectLinks(path);
        try
        {
            var settings = JsonNode.Parse(await File.ReadAllTextAsync(path, cancellationToken)) as JsonObject
                ?? throw new InvalidDataException(UiText.Get("存储设置文件格式无效，原文件已保留。"));
            if (!settings.TryGetPropertyValue("Storage", out var storage)) return;
            if (storage is not JsonObject storageObject) throw new InvalidDataException(UiText.Get("存储版本设置格式无效。"));
            if (storageObject.TryGetPropertyValue("LayoutVersion", out var version) &&
                (version is not JsonValue value || !value.TryGetValue<int>(out int number) || number != 1))
                throw new InvalidDataException(UiText.Get("此数据目录的结构版本不受当前程序支持，请使用兼容版本打开。"));
        }
        catch (JsonException error) { throw new InvalidDataException(UiText.Get("存储设置文件格式无效，原文件已保留。"), error); }
    }

    // Metadata may also be present in a pending catalog transaction. Rewrite only
    // managed work folders; external folders and immutable backup snapshots stay put.
    private static void RelocateProjectFolders(JsonNode node, string desktop, string target)
    {
        if (node is JsonObject obj)
            foreach (var pair in obj.ToArray())
            {
                if (string.Equals(pair.Key, "FolderPath", StringComparison.OrdinalIgnoreCase)
                    && pair.Value is JsonValue value && value.TryGetValue<string>(out string? folder)
                    && !string.IsNullOrWhiteSpace(folder) && Path.IsPathFullyQualified(folder)
                    && IsWithin(folder, Path.Combine(desktop, "Projects")))
                    obj[pair.Key] = Path.Combine(target, "Desktop", Path.GetRelativePath(desktop, folder));
                else if (pair.Value is JsonObject or JsonArray) RelocateProjectFolders(pair.Value, desktop, target);
            }
        else if (node is JsonArray array)
            foreach (var child in array)
                if (child is JsonObject or JsonArray) RelocateProjectFolders(child, desktop, target);
    }

    internal static string[] ListFiles(CopyRoot copy, bool directories = false)
    {
        string root = copy.Source;
        RejectLinks(root);
        if (File.Exists(root)) throw new IOException(UiText.Get("存储设置文件格式无效，原文件已保留。"));
        if (!Directory.Exists(root)) return [];
        var files = new List<string>();
        void Visit(string directory)
        {
            foreach (var path in Directory.EnumerateFileSystemEntries(directory))
            {
                if (directory == root && ((copy.Include is not null && !copy.Include.Contains(Path.GetFileName(path), StringComparer.OrdinalIgnoreCase))
                    || (copy.Exclude is not null && copy.Exclude.Contains(Path.GetFileName(path), StringComparer.OrdinalIgnoreCase)))) continue;
                var attributes = File.GetAttributes(path);
                if (attributes.HasFlag(FileAttributes.ReparsePoint)) throw new IOException(UiText.Get("数据目录中包含链接，请先移除链接或单独迁移。"));
                if (attributes.HasFlag(FileAttributes.Directory))
                {
                    if (directories) files.Add(Path.GetRelativePath(root, path));
                    Visit(path);
                }
                else
                {
                    RejectLinks(path);
                    if (!directories) files.Add(Path.GetRelativePath(root, path));
                }
            }
        }
        Visit(root);
        return files.Order(StringComparer.OrdinalIgnoreCase).ToArray();
    }

    internal static void RejectLinks(string path)
    {
        for (string? part = Path.GetFullPath(path); part is not null; part = Path.GetDirectoryName(part))
            try
            {
                if (File.GetAttributes(part).HasFlag(FileAttributes.ReparsePoint))
                    throw new IOException(UiText.Get("迁移路径不能经过符号链接或目录联接。"));
            }
            catch (FileNotFoundException) { }
            catch (DirectoryNotFoundException) { }
        if (OperatingSystem.IsWindows() && File.Exists(path))
        {
            using var handle = File.OpenHandle(path, FileMode.Open, FileAccess.Read, FileShare.Read);
            if (!GetFileInformationByHandle(handle, out var information)) throw new IOException(UiText.Get("文件复制校验失败，原数据未改动。"));
            if (information.NumberOfLinks != 1) throw new IOException(UiText.Get("数据目录中包含链接，请先移除链接或单独迁移。"));
        }
    }

    internal static async Task<byte[]> HashAsync(string path, CancellationToken cancellationToken)
    {
        RejectLinks(path);
        await using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read);
        return await SHA256.HashDataAsync(stream, cancellationToken);
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetFileInformationByHandle(SafeFileHandle file, out NativeFileInformation information);

    [StructLayout(LayoutKind.Sequential)]
    private struct NativeFileInformation
    {
        public uint Attributes;
        public System.Runtime.InteropServices.ComTypes.FILETIME CreationTime, LastAccessTime, LastWriteTime;
        public uint VolumeSerialNumber, FileSizeHigh, FileSizeLow, NumberOfLinks, FileIndexHigh, FileIndexLow;
    }
}
