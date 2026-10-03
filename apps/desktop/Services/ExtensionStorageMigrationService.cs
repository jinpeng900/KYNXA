using System.Text.Json;

namespace KYNXA_Desktop.Services;

public sealed record ExtensionStorageMigrationResult(string ExtensionRoot, int VerifiedFiles);

/// <summary>The caller pauses gateway writers under storage-migration.lock; sources remain recoverable.</summary>
public static class ExtensionStorageMigrationService
{
    public static async Task<ExtensionStorageMigrationResult> MoveAsync(string source, string target, string pointer,
        IProgress<string>? progress = null, CancellationToken cancellationToken = default)
    {
        cancellationToken.ThrowIfCancellationRequested();
        source = ExtensionPaths.NormalizeRoot(source);
        target = ExtensionPaths.NormalizeRoot(target);
        pointer = ExtensionPaths.NormalizePointer(pointer);
        StorageMigrationService.RejectLinks(pointer);
        if (StorageMigrationService.IsWithin(target, source) || StorageMigrationService.IsWithin(source, target))
            throw new InvalidOperationException(UiText.Get("新目录不能与原数据目录相同、包含原目录或位于原目录内部。"));
        if (StorageMigrationService.IsWithin(pointer, target))
            throw new InvalidOperationException(UiText.Get("新目录不能与原数据目录相同、包含原目录或位于原目录内部。"));
        if (File.Exists(target) || Directory.Exists(target) && Directory.EnumerateFileSystemEntries(target).Any())
            throw new InvalidOperationException(UiText.Get("目标文件夹不是空的，请选择空文件夹，避免覆盖已有数据。"));
        Directory.CreateDirectory(Path.GetDirectoryName(pointer)!);
        string operationLock = Path.Combine(Path.GetDirectoryName(pointer)!, "extensions-operation.lock");
        StorageMigrationService.RejectLinks(operationLock);
        await using var lockStream = new FileStream(operationLock, FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None);
        if (Directory.Exists(pointer)) throw new InvalidDataException(UiText.Get("扩展配置格式无效，原文件已保留。"));
        byte[]? oldPointer = File.Exists(pointer) ? await File.ReadAllBytesAsync(pointer, cancellationToken) : null;
        if (oldPointer is not null)
        {
            string oldRoot = ExtensionPaths.ParsePointer(oldPointer);
            if (!string.Equals(oldRoot, source, StringComparison.OrdinalIgnoreCase))
                throw new IOException(UiText.Get("扩展配置已被其他实例修改，请重新打开设置。"));
        }
        ExtensionPaths.ValidateLayout(source);
        await ExtensionConfigurationMigration.ValidateAsync(source, cancellationToken);
        var roots = new List<StorageMigrationService.CopyRoot>(new[] { "Agent", "Skills", "MCP" }
            .Select(name => new StorageMigrationService.CopyRoot(Path.Combine(source, name), name)))
        {
            // Data may share this root: only the extension-owned recovery namespace moves.
            new(Path.Combine(source, "Backups", "Extensions"), Path.Combine("Backups", "Extensions")),
            new(source, "", Include: ["extension-layout.json"])
        };
        string[] historicalMetadata = ["extension-migration-info.json", "extensions-pointer.previous.json"];
        if (historicalMetadata.Any(name => File.Exists(Path.Combine(source, name))))
            roots.Add(new(source, Path.Combine("Backups", "Extensions", "Migrations", Guid.NewGuid().ToString("N")), Include: historicalMetadata));
        var snapshots = roots.Select(root => (Root: root, Existed: Directory.Exists(root.Source),
            Files: StorageMigrationService.ListFiles(root), Directories: StorageMigrationService.ListFiles(root, true))).ToArray();
        Directory.CreateDirectory(target);
        var verified = new List<(string Source, string Destination, byte[] Hash)>();
        foreach (var snapshot in snapshots)
        {
            string destinationRoot = Path.Combine(target, snapshot.Root.Name);
            Directory.CreateDirectory(destinationRoot);
            foreach (string relative in snapshot.Directories)
                Directory.CreateDirectory(Path.Combine(destinationRoot, relative));
            foreach (string relative in snapshot.Files)
            {
                cancellationToken.ThrowIfCancellationRequested();
                progress?.Report(string.Format(UiText.Get("正在复制并校验文件（{0}）…"), verified.Count + 1));
                string original = Path.Combine(snapshot.Root.Source, relative), copied = Path.Combine(destinationRoot, relative);
                StorageMigrationService.RejectLinks(original);
                StorageMigrationService.RejectLinks(copied);
                byte[] hash = await StorageMigrationService.HashAsync(original, cancellationToken);
                Directory.CreateDirectory(Path.GetDirectoryName(copied)!);
                await using (var input = new FileStream(original, FileMode.Open, FileAccess.Read, FileShare.Read))
                await using (var output = new FileStream(copied, FileMode.CreateNew, FileAccess.Write, FileShare.None))
                    await input.CopyToAsync(output, cancellationToken);
                byte[] copiedHash = await StorageMigrationService.HashAsync(copied, cancellationToken);
                if (!hash.SequenceEqual(copiedHash))
                    throw new IOException(UiText.Get("文件复制校验失败，原数据未改动。"));
                verified.Add((original, copied, hash));
            }
        }
        progress?.Report(UiText.Get("正在核对数据并更新内置项目路径…"));
        await VerifySourceAsync();
        var copiedFiles = snapshots.SelectMany(snapshot => snapshot.Files.Select(name => Path.Combine(snapshot.Root.Name, name))).ToHashSet(StringComparer.OrdinalIgnoreCase);
        var copiedDirectories = snapshots.SelectMany(snapshot => snapshot.Directories.Select(name => Path.Combine(snapshot.Root.Name, name)))
            .Concat(snapshots.Select(snapshot => snapshot.Root.Name).Where(name => name.Length > 0)).ToHashSet(StringComparer.OrdinalIgnoreCase);
        AddParentDirectories(copiedDirectories, copiedFiles);
        var targetCopy = new StorageMigrationService.CopyRoot(target, "");
        if (!copiedFiles.SetEquals(StorageMigrationService.ListFiles(targetCopy))
            || !copiedDirectories.SetEquals(StorageMigrationService.ListFiles(targetCopy, true)))
            throw new IOException(UiText.Get("文件复制校验失败，原数据未改动。"));
        foreach (var file in verified)
        {
            byte[] digest = await StorageMigrationService.HashAsync(file.Destination, cancellationToken);
            if (!file.Hash.SequenceEqual(digest))
                throw new IOException(UiText.Get("文件复制校验失败，原数据未改动。"));
        }
        await ExtensionConfigurationMigration.RelocateAgentConfigAsync(source, target, cancellationToken);
        await ExtensionConfigurationMigration.ValidateAsync(target, cancellationToken);
        ExtensionPaths.EnsureLayout(target);
        if (oldPointer is not null) await File.WriteAllBytesAsync(Path.Combine(target, "extensions-pointer.previous.json"), oldPointer, cancellationToken);
        await File.WriteAllTextAsync(Path.Combine(target, "extension-migration-info.json"), JsonSerializer.Serialize(new
        { version = 1, completedAt = DateTimeOffset.UtcNow, verifiedFiles = verified.Count, originalFilesRetained = true }), cancellationToken);
        var expectedFiles = snapshots.SelectMany(snapshot => snapshot.Files.Select(name => Path.Combine(snapshot.Root.Name, name)))
            .Append("extension-layout.json").Append("extension-migration-info.json")
            .Concat(oldPointer is null ? [] : new[] { "extensions-pointer.previous.json" }).ToHashSet(StringComparer.OrdinalIgnoreCase);
        var expectedDirectories = ExtensionPaths.LayoutDirectories.Select(name => name.Replace('/', Path.DirectorySeparatorChar))
            .Concat(snapshots.SelectMany(snapshot => snapshot.Directories.Select(name => Path.Combine(snapshot.Root.Name, name))))
            .Concat(snapshots.Select(snapshot => snapshot.Root.Name).Where(name => name.Length > 0))
            .ToHashSet(StringComparer.OrdinalIgnoreCase);
        AddParentDirectories(expectedDirectories, expectedFiles);
        VerifyDestinationLayout();
        // Hashes cover deliberately rewritten config and new recovery metadata as well.
        var finalHashes = new List<(string Path, byte[] Hash)>();
        foreach (string relative in expectedFiles)
        {
            string path = Path.Combine(target, relative);
            finalHashes.Add((path, await StorageMigrationService.HashAsync(path, cancellationToken)));
        }
        await VerifySourceAsync();
        VerifyDestinationLayout();
        foreach (var file in finalHashes)
        {
            byte[] digest = await StorageMigrationService.HashAsync(file.Path, cancellationToken);
            if (!file.Hash.SequenceEqual(digest))
                throw new IOException(UiText.Get("文件复制校验失败，原数据未改动。"));
        }
        StorageMigrationService.RejectLinks(pointer);
        byte[]? currentPointer = File.Exists(pointer) ? await File.ReadAllBytesAsync(pointer, cancellationToken) : null;
        if (!(oldPointer ?? []).SequenceEqual(currentPointer ?? []))
            throw new IOException(UiText.Get("扩展配置已被其他实例修改，请重新打开设置。"));
        string temporary = pointer + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            await File.WriteAllTextAsync(temporary, JsonSerializer.Serialize(new { version = 1, extensionRoot = target }), cancellationToken);
            cancellationToken.ThrowIfCancellationRequested();
            // No cancellation-sensitive operation follows this atomic activation boundary.
            File.Move(temporary, pointer, overwrite: true);
        }
        finally { if (File.Exists(temporary)) File.Delete(temporary); }
        return new(target, verified.Count);

        async Task VerifySourceAsync()
        {
            foreach (var snapshot in snapshots)
                if (snapshot.Existed != Directory.Exists(snapshot.Root.Source)
                    || !snapshot.Files.SequenceEqual(StorageMigrationService.ListFiles(snapshot.Root))
                    || !snapshot.Directories.SequenceEqual(StorageMigrationService.ListFiles(snapshot.Root, true)))
                    throw new IOException(UiText.Get("扩展迁移期间配置发生变化，请停止其他 KYNXA 实例后重试。"));
            foreach (var file in verified)
            {
                byte[] digest = await StorageMigrationService.HashAsync(file.Source, cancellationToken);
                if (!file.Hash.SequenceEqual(digest))
                    throw new IOException(UiText.Get("扩展迁移期间配置发生变化，请停止其他 KYNXA 实例后重试。"));
            }
        }

        void VerifyDestinationLayout()
        {
            var copy = new StorageMigrationService.CopyRoot(target, "");
            if (!expectedFiles.SetEquals(StorageMigrationService.ListFiles(copy))
                || !expectedDirectories.SetEquals(StorageMigrationService.ListFiles(copy, true)))
                throw new IOException(UiText.Get("文件复制校验失败，原数据未改动。"));
            ExtensionPaths.ValidateLayout(target);
        }

        static void AddParentDirectories(HashSet<string> directories, IEnumerable<string> files)
        {
            foreach (string path in directories.Concat(files).ToArray())
                for (string? parent = Path.GetDirectoryName(path); !string.IsNullOrEmpty(parent); parent = Path.GetDirectoryName(parent))
                    directories.Add(parent);
        }
    }
}
