namespace KYNXA.ToolHost;

internal sealed class WorkspaceSnapshot
{
    internal const int MaximumFiles = 512;
    internal const int MaximumEntries = 2048;
    internal const long MaximumBytes = 32 * 1024 * 1024;
    internal const long MaximumFileBytes = 4 * 1024 * 1024;
    private static readonly HashSet<string> ExcludedDirectories = new(StringComparer.OrdinalIgnoreCase)
    {
        "Data", "Models", "Backups", "Trash", "Index", "Chats", "Memory", ".kynxa", ".codex", ".ssh", ".aws", ".azure", ".docker", ".kube", ".config",
        ".git", ".svn", ".hg", "node_modules", "bin", "obj", "dist", "build", ".vs", ".venv", "venv",
        ".sandbox-runtime", ".sandbox-temp", ".sandbox-skill"
    };
    private readonly string[] _excludedRoots;
    private string _sourceRoot = "";
    private int _entries;
    private readonly bool _trustedManagedWorkspace;
    private bool _allowDataAncestor;
    internal int Files { get; private set; }
    internal long Bytes { get; private set; }
    internal int Skipped { get; private set; }

    internal WorkspaceSnapshot(IEnumerable<string> excludedRoots, bool trustedManagedWorkspace)
    {
        _excludedRoots = excludedRoots.Select(Path.GetFullPath).ToArray();
        _trustedManagedWorkspace = trustedManagedWorkspace;
    }

    internal void Copy(string source, string destination)
    {
        _sourceRoot = Path.GetFullPath(source);
        _allowDataAncestor = _trustedManagedWorkspace && _excludedRoots.Any(IsManagedFolderBelow);
        if (_trustedManagedWorkspace && !_allowDataAncestor)
            throw new SandboxException("SANDBOX_INVALID_WORKSPACE", "A trusted managed workspace must be a canonical Data/Desktop/Projects/<project-id> folder.");
        if (IsExcludedRoot(source)) throw new SandboxException("SANDBOX_INVALID_WORKSPACE", "The application data directory cannot be used as a terminal workspace.");
        if (!Directory.Exists(source)) throw new SandboxException("SANDBOX_INVALID_WORKSPACE", "Workspace directory does not exist.");
        EnsureNoReparseAncestors(source);
        CopyDirectory(source, destination, 0);
    }

    private void CopyDirectory(string source, string destination, int depth)
    {
        if (depth > 32) throw new SandboxException("SANDBOX_SNAPSHOT_LIMIT", "Workspace nesting exceeds the snapshot limit.");
        Directory.CreateDirectory(destination);
        foreach (string path in Directory.EnumerateFileSystemEntries(source))
        {
            if (++_entries > MaximumEntries)
                throw new SandboxException("SANDBOX_SNAPSHOT_LIMIT", "Workspace entry count exceeds the bounded snapshot limit.");
            string name = Path.GetFileName(path);
            var attributes = File.GetAttributes(path);
            if ((attributes & FileAttributes.ReparsePoint) != 0 || IsExcludedRoot(path)) { Skipped++; continue; }
            if ((attributes & FileAttributes.Directory) != 0)
            {
                if (ExcludedDirectories.Contains(name)) { Skipped++; continue; }
                CopyDirectory(path, Path.Combine(destination, name), depth + 1);
                continue;
            }
            if (IsSensitiveFile(name)) { Skipped++; continue; }
            var file = new FileInfo(path);
            if (file.Length > MaximumFileBytes) { Skipped++; continue; }
            if (++Files > MaximumFiles || Bytes + file.Length > MaximumBytes)
                throw new SandboxException("SANDBOX_SNAPSHOT_LIMIT", "Workspace exceeds the bounded sandbox snapshot limit.");
            // Disallow concurrent writers during the copy; recheck reparse attributes after opening.
            using var input = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read);
            NativeMethods.Check(NativeMethods.GetFileInformationByHandle(input.SafeFileHandle, out var information), "GetFileInformationByHandle snapshot");
            if (information.NumberOfLinks > 1) { Files--; Skipped++; continue; }
            var finalPath = new System.Text.StringBuilder(32768);
            uint finalLength = NativeMethods.GetFinalPathNameByHandleW(input.SafeFileHandle, finalPath, (uint)finalPath.Capacity, 0);
            NativeMethods.Check(finalLength > 0 && finalLength < finalPath.Capacity, "GetFinalPathNameByHandle snapshot");
            string resolved = finalPath.ToString();
            if (resolved.StartsWith("\\\\?\\", StringComparison.Ordinal)) resolved = resolved[4..];
            if (!IsWithin(_sourceRoot, resolved) || IsExcludedRoot(resolved))
                throw new SandboxException("SANDBOX_INVALID_WORKSPACE", "An opened snapshot file resolves outside the authorized workspace.");
            if ((File.GetAttributes(path) & FileAttributes.ReparsePoint) != 0)
                throw new SandboxException("SANDBOX_INVALID_WORKSPACE", "Workspace changed to a link during snapshot creation.");
            if (input.Length > MaximumFileBytes || Bytes + input.Length > MaximumBytes)
                throw new SandboxException("SANDBOX_SNAPSHOT_LIMIT", "Workspace changed beyond the snapshot limit.");
            using var output = new FileStream(Path.Combine(destination, name), FileMode.CreateNew, FileAccess.Write, FileShare.None);
            input.CopyTo(output);
            Bytes += input.Length;
        }
    }

    private bool IsExcludedRoot(string path)
    {
        foreach (string root in _excludedRoots)
        {
            if (!IsWithin(root, path)) continue;
            // Only the service's canonical managed work folder may bypass its Data ancestor.
            // Explicit exclusions inside the work folder still apply, as do all sensitive-name filters.
            if (_allowDataAncestor && IsWithin(root, _sourceRoot) && !PathEquals(root, _sourceRoot) && IsWithin(_sourceRoot, path)) continue;
            return true;
        }
        return false;
    }

    private bool IsManagedFolderBelow(string root)
    {
        if (!IsWithin(root, _sourceRoot)) return false;
        string[] parts = Path.GetRelativePath(root, _sourceRoot).Split(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        return parts.Length == 3 && parts[0].Equals("Desktop", StringComparison.OrdinalIgnoreCase) &&
            parts[1].Equals("Projects", StringComparison.OrdinalIgnoreCase) && Guid.TryParse(parts[2], out _);
    }

    private static bool PathEquals(string first, string second) =>
        Path.TrimEndingDirectorySeparator(Path.GetFullPath(first)).Equals(Path.TrimEndingDirectorySeparator(Path.GetFullPath(second)), StringComparison.OrdinalIgnoreCase);

    internal static bool IsWithin(string root, string path)
    {
        root = Path.TrimEndingDirectorySeparator(Path.GetFullPath(root));
        path = Path.GetFullPath(path);
        return path.Equals(root, StringComparison.OrdinalIgnoreCase) || path.StartsWith(root + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase);
    }

    internal static bool IsSensitiveFile(string name)
    {
        string lower = name.ToLowerInvariant();
        string extension = Path.GetExtension(lower);
        return lower.StartsWith(".env", StringComparison.Ordinal) || lower.Contains("credential", StringComparison.Ordinal) ||
            lower.Contains("secret", StringComparison.Ordinal) || lower is "id_rsa" or "id_ed25519" or "storage.json" or "settings.json" or "events.jsonl" or
                "auth.json" or ".npmrc" or ".pypirc" or ".netrc" or ".git-credentials" ||
            extension is ".pem" or ".key" or ".pfx" or ".p12" or ".keystore" or ".jks" or ".kdbx";
    }

    private static void EnsureNoReparseAncestors(string path)
    {
        for (DirectoryInfo? directory = new(Path.GetFullPath(path)); directory is not null; directory = directory.Parent)
            if ((directory.Attributes & FileAttributes.ReparsePoint) != 0)
                throw new SandboxException("SANDBOX_INVALID_WORKSPACE", "A workspace cannot be a linked directory or be beneath one.");
    }

    internal static void DeleteOwnedRun(string path)
    {
        string parent = Path.Combine(Path.GetTempPath(), "kynxa-tool-sandbox");
        if (!IsWithin(parent, path) || Path.GetFullPath(path).Equals(Path.GetFullPath(parent), StringComparison.OrdinalIgnoreCase))
            throw new InvalidOperationException("Refusing cleanup outside the owned sandbox run directory.");
        if (Directory.Exists(path)) Directory.Delete(path, recursive: true);
    }
}
