using System.Security.Cryptography;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace KYNXA_Desktop.Services;

public sealed record StorageMigrationResult(string DataRoot, int VerifiedFiles);

/// <summary>Copy and verify while all application writers are paused; activate the pointer last.</summary>
public static class StorageMigrationService
{
    private static readonly string[] ConversationEntries =
        ["Projects", "Chats", "Trash", "Backups", "Memory", "Index", "settings.json", "catalog.json", ".conversations-v1.json", ".catalog-transaction.json"];

    private sealed record CopyRoot(string Source, string Name, string[]? Include = null, string[]? Exclude = null);

    public static bool IsWithin(string path, string parent) =>
        string.Equals(Path.TrimEndingDirectorySeparator(Path.GetFullPath(path)), Path.TrimEndingDirectorySeparator(Path.GetFullPath(parent)), StringComparison.OrdinalIgnoreCase)
        || Path.GetFullPath(path).StartsWith(Path.TrimEndingDirectorySeparator(Path.GetFullPath(parent)) + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase);

    public static async Task<StorageMigrationResult> MoveAsync(string desktop, string models, string target,
        string pointer, IProgress<string>? progress = null, CancellationToken cancellationToken = default,
        Func<string, CancellationToken, Task>? initializeTarget = null)
    {
        if (!Path.IsPathFullyQualified(target) || target.StartsWith(@"\\"))
            throw new InvalidOperationException("请选择本机磁盘上的绝对路径。");
        target = Path.TrimEndingDirectorySeparator(Path.GetFullPath(target));
        if (target == Path.GetPathRoot(target)) throw new InvalidOperationException("不能直接使用磁盘根目录，请选择一个空文件夹。");
        models = Path.TrimEndingDirectorySeparator(Path.GetFullPath(models));
        desktop = Path.TrimEndingDirectorySeparator(Path.GetFullPath(desktop));
        bool standardModelsDirectory = string.Equals(Path.GetFileName(models), "Models", StringComparison.OrdinalIgnoreCase);
        string conversationRoot = standardModelsDirectory ? Path.GetDirectoryName(models)! : Path.Combine(models, "Conversations");
        foreach (var source in new[] { desktop, models }.Concat(ConversationEntries.Select(name => Path.Combine(conversationRoot, name))))
            if (IsWithin(target, source) || IsWithin(source, target))
                throw new InvalidOperationException("新目录不能与原数据目录相同、包含原目录或位于原目录内部。");
        RejectLinks(target);
        if (Directory.Exists(target) && Directory.EnumerateFileSystemEntries(target).Any())
            throw new InvalidOperationException("目标文件夹不是空的，请选择空文件夹，避免覆盖已有数据。");
        await ValidateLayoutSettingsAsync(conversationRoot, cancellationToken);
        byte[]? oldPointer = File.Exists(pointer) ? await File.ReadAllBytesAsync(pointer, cancellationToken) : null;
        // A custom model home keeps conversations below itself; move that subtree to
        // the new Data root exactly once, where the standard Models layout expects it.
        var roots = new[] {
            new CopyRoot(desktop, "Desktop"),
            new CopyRoot(models, "Models", Exclude: standardModelsDirectory ? null : ["Conversations"]),
            new CopyRoot(conversationRoot, "", Include: ConversationEntries)
        };
        var snapshots = roots.Select(root => (Root: root, Files: ListFiles(root), Directories: ListFiles(root, true))).ToArray();
        Directory.CreateDirectory(target);
        var verified = new List<(string Source, byte[] Hash)>();
        foreach (var root in snapshots)
        {
            Directory.CreateDirectory(Path.Combine(target, root.Root.Name));
            foreach (var directory in root.Directories)
                Directory.CreateDirectory(Path.Combine(target, root.Root.Name, directory));
            foreach (var relative in root.Files)
            {
                cancellationToken.ThrowIfCancellationRequested();
                progress?.Report($"正在复制并校验文件（{verified.Count + 1}）…");
                string source = Path.Combine(root.Root.Source, relative), destination = Path.Combine(target, root.Root.Name, relative);
                RejectLinks(source);
                byte[] digest = await HashAsync(source, cancellationToken);
                Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
                await using (var input = new FileStream(source, FileMode.Open, FileAccess.Read, FileShare.Read))
                await using (var output = new FileStream(destination, FileMode.CreateNew, FileAccess.Write, FileShare.None))
                    await input.CopyToAsync(output, cancellationToken);
                byte[] copiedDigest = await HashAsync(destination, cancellationToken);
                if (!digest.SequenceEqual(copiedDigest)) throw new IOException("文件复制校验失败，原数据未改动。");
                verified.Add((source, digest));
            }
        }
        progress?.Report("正在核对数据并更新内置项目路径…");
        foreach (var root in snapshots)
            if (!root.Files.SequenceEqual(ListFiles(root.Root)) || !root.Directories.SequenceEqual(ListFiles(root.Root, true)))
                throw new IOException("迁移期间原目录发生变化，请停止其他 KYNXA 实例后重试。");
        foreach (var file in verified)
        {
            byte[] currentDigest = await HashAsync(file.Source, cancellationToken);
            if (!file.Hash.SequenceEqual(currentDigest)) throw new IOException("迁移期间文件被修改，尚未切换存储位置。");
        }

        string projectsPath = Path.Combine(target, "Desktop", "projects.json");
        if (File.Exists(projectsPath))
        {
            var projects = JsonNode.Parse(await File.ReadAllTextAsync(projectsPath, cancellationToken))?.AsArray()
                ?? throw new InvalidDataException("项目数据无效。");
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
                ?? throw new InvalidDataException("会话目录数据无效。");
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
                    ?? throw new InvalidDataException("项目清单数据无效。");
                RelocateProjectFolders(document, desktop, target);
                await File.WriteAllTextAsync(path, document.ToJsonString(new JsonSerializerOptions { WriteIndented = true }), cancellationToken);
            }
        string localServerPath = Path.Combine(target, "Models", "local-server.json");
        if (File.Exists(localServerPath))
        {
            var local = JsonNode.Parse(await File.ReadAllTextAsync(localServerPath, cancellationToken))?.AsObject()
                ?? throw new InvalidDataException("本地模型配置无效。");
            foreach (string key in new[] { "modelPath", "serverPath", "backendPath" })
                if (local[key]?.GetValue<string>() is string path)
                    foreach (var root in roots.Where(root => root.Name.Length > 0))
                        if (IsWithin(path, root.Source)) local[key] = Path.Combine(target, root.Name, Path.GetRelativePath(root.Source, path));
            await File.WriteAllTextAsync(localServerPath, local.ToJsonString(new JsonSerializerOptions { WriteIndented = true }), cancellationToken);
        }
        // Refuse an incompatible/corrupt destination before making it active.
        await ValidateLayoutSettingsAsync(target, cancellationToken);
        if (initializeTarget is not null)
        {
            progress?.Report("正在初始化新目录并检查会话记录…");
            await initializeTarget(target, cancellationToken);
            await ValidateLayoutSettingsAsync(target, cancellationToken);
        }
        cancellationToken.ThrowIfCancellationRequested();
        byte[]? currentPointer = File.Exists(pointer) ? await File.ReadAllBytesAsync(pointer, cancellationToken) : null;
        if (!(oldPointer ?? []).SequenceEqual(currentPointer ?? [])) throw new IOException("存储配置被其他实例修改，请重新打开设置。");
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
    }

    private static async Task ValidateLayoutSettingsAsync(string root, CancellationToken cancellationToken)
    {
        string path = Path.Combine(root, "settings.json");
        if (!File.Exists(path))
        {
            if (Directory.Exists(path)) throw new InvalidDataException("存储设置文件路径被文件夹占用。");
            return;
        }
        RejectLinks(path);
        try
        {
            var settings = JsonNode.Parse(await File.ReadAllTextAsync(path, cancellationToken)) as JsonObject
                ?? throw new InvalidDataException("存储设置文件格式无效，原文件已保留。");
            if (!settings.TryGetPropertyValue("Storage", out var storage)) return;
            if (storage is not JsonObject storageObject) throw new InvalidDataException("存储版本设置格式无效。");
            if (storageObject.TryGetPropertyValue("LayoutVersion", out var version) &&
                (version is not JsonValue value || !value.TryGetValue<int>(out int number) || number != 1))
                throw new InvalidDataException("此数据目录的结构版本不受当前程序支持，请使用兼容版本打开。");
        }
        catch (JsonException error) { throw new InvalidDataException("存储设置文件格式无效，原文件已保留。", error); }
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

    private static string[] ListFiles(CopyRoot copy, bool directories = false)
    {
        string root = copy.Source;
        RejectLinks(root);
        if (!Directory.Exists(root)) return [];
        var files = new List<string>();
        void Visit(string directory)
        {
            foreach (var path in Directory.EnumerateFileSystemEntries(directory))
            {
                if (directory == root && ((copy.Include is not null && !copy.Include.Contains(Path.GetFileName(path), StringComparer.OrdinalIgnoreCase))
                    || (copy.Exclude is not null && copy.Exclude.Contains(Path.GetFileName(path), StringComparer.OrdinalIgnoreCase)))) continue;
                var attributes = File.GetAttributes(path);
                if (attributes.HasFlag(FileAttributes.ReparsePoint)) throw new IOException("数据目录中包含链接，请先移除链接或单独迁移。");
                if (attributes.HasFlag(FileAttributes.Directory))
                {
                    if (directories) files.Add(Path.GetRelativePath(root, path));
                    Visit(path);
                }
                else if (!directories) files.Add(Path.GetRelativePath(root, path));
            }
        }
        Visit(root);
        return files.Order(StringComparer.OrdinalIgnoreCase).ToArray();
    }

    private static void RejectLinks(string path)
    {
        for (string? part = Path.GetFullPath(path); part is not null; part = Path.GetDirectoryName(part))
            if ((Directory.Exists(part) || File.Exists(part)) && File.GetAttributes(part).HasFlag(FileAttributes.ReparsePoint))
                throw new IOException("迁移路径不能经过符号链接或目录联接。");
    }

    private static async Task<byte[]> HashAsync(string path, CancellationToken cancellationToken)
    {
        await using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read);
        return await SHA256.HashDataAsync(stream, cancellationToken);
    }
}
