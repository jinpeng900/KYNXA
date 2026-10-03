using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace KYNXA_Desktop.Services;

public sealed class WorkspaceFileEntry
{
    internal WorkspaceFileEntry(string path, string name, bool isDirectory, long length)
    {
        FullPath = path;
        Name = name;
        IsDirectory = isDirectory;
        Length = length;
    }

    public string FullPath { get; }
    public string Name { get; }
    public bool IsDirectory { get; }
    public long Length { get; }
    public string Description => IsDirectory ? "文件夹 · 仅显示，不展开"
        : $"{(Path.GetExtension(Name) is string extension && extension.Length > 0 ? extension : "无扩展名")} 文件 · {FormatSize(Length)}";

    private static string FormatSize(long bytes) => bytes < 1024 ? $"{bytes} B"
        : bytes < 1024 * 1024 ? $"{bytes / 1024d:0.#} KiB" : $"{bytes / (1024d * 1024):0.#} MiB";
}

public enum WorkspaceDirectoryStatus { Ready, NotAssociated, Missing, Empty, Failed }
public sealed record WorkspaceDirectoryResult(WorkspaceDirectoryStatus Status, IReadOnlyList<WorkspaceFileEntry> Entries, string Message);
public sealed record WorkspaceTextResult(bool Success, string Text, string Message);

/// <summary>Reads only a selected workspace's first level. It never executes, writes or uploads files.</summary>
public static class WorkspaceFileReader
{
    public const int MaximumEntries = 200;
    public const int MaximumTextBytes = 256 * 1024;
    private const int MaximumScannedEntries = 2000;
    private static readonly HashSet<string> TextExtensions = new(StringComparer.OrdinalIgnoreCase)
    {
        ".txt", ".md", ".markdown", ".cs", ".xaml", ".js", ".mjs", ".cjs", ".ts", ".tsx", ".jsx",
        ".json", ".jsonc", ".html", ".htm", ".css", ".scss", ".xml", ".yaml", ".yml", ".toml",
        ".py", ".java", ".c", ".h", ".cpp", ".hpp", ".sql", ".sh", ".ps1", ".bat", ".cmd",
        ".rs", ".go", ".vue", ".svelte", ".ini", ".cfg", ".conf", ".csv", ".log", ".tex",
        ".csproj", ".props", ".targets", ".sln", ".slnx", ".gitignore", ".gitattributes", ".editorconfig"
    };

    public static Task<WorkspaceDirectoryResult> ListAsync(string? rootPath, CancellationToken cancellationToken = default) =>
        Task.Run(() => List(rootPath, cancellationToken), cancellationToken);

    private static WorkspaceDirectoryResult List(string? rootPath, CancellationToken cancellationToken)
    {
        if (string.IsNullOrWhiteSpace(rootPath))
            return new(WorkspaceDirectoryStatus.NotAssociated, [], "此工作没有关联文件夹，请先关联本地目录。");
        try
        {
            string root = ValidateRoot(rootPath);
            var entries = new List<WorkspaceFileEntry>();
            int scanned = 0;
            int skipped = 0;
            bool limited = false;
            foreach (string path in Directory.EnumerateFileSystemEntries(root))
            {
                cancellationToken.ThrowIfCancellationRequested();
                if (++scanned > MaximumScannedEntries) { limited = true; break; }
                try
                {
                    EnsureDirectChild(root, path);
                    var attributes = File.GetAttributes(path);
                    if ((attributes & (FileAttributes.Hidden | FileAttributes.System | FileAttributes.ReparsePoint)) != 0)
                        continue;
                    bool directory = (attributes & FileAttributes.Directory) != 0;
                    long length = directory ? 0 : new FileInfo(path).Length;
                    entries.Add(new WorkspaceFileEntry(path, Path.GetFileName(path), directory, length));
                    if (entries.Count == MaximumEntries) { limited = true; break; }
                }
                catch (Exception error) when (IsFileError(error)) { skipped++; }
            }
            entries.Sort((left, right) => left.IsDirectory != right.IsDirectory
                ? left.IsDirectory ? -1 : 1 : StringComparer.OrdinalIgnoreCase.Compare(left.Name, right.Name));
            string status = entries.Count == 0 ? limited ? "本次扫描未找到可显示文件。" : "此目录中没有可显示的文件。" : $"{entries.Count} 项 · 仅当前目录";
            if (limited) status += " · 目录较大，仅显示部分内容";
            if (skipped > 0) status += $" · {skipped} 项无法读取";
            return new(entries.Count == 0 ? WorkspaceDirectoryStatus.Empty : WorkspaceDirectoryStatus.Ready, entries, status);
        }
        catch (DirectoryNotFoundException) { return new(WorkspaceDirectoryStatus.Missing, [], "关联目录不存在或已被移动，请重新关联。"); }
        catch (FileNotFoundException) { return new(WorkspaceDirectoryStatus.Missing, [], "关联目录不存在或已被移动，请重新关联。"); }
        catch (Exception error) when (IsFileError(error))
        {
            return new(WorkspaceDirectoryStatus.Failed, [], "无法读取此目录：" + error.Message);
        }
    }

    public static Task<WorkspaceTextResult> ReadTextAsync(string? rootPath, string filePath, CancellationToken cancellationToken = default) =>
        Task.Run(async () => await ReadTextCoreAsync(rootPath, filePath, cancellationToken).ConfigureAwait(false), cancellationToken);

    private static async Task<WorkspaceTextResult> ReadTextCoreAsync(string? rootPath, string filePath, CancellationToken cancellationToken)
    {
        try
        {
            cancellationToken.ThrowIfCancellationRequested();
            string root = ValidateRoot(rootPath);
            if (!Path.IsPathFullyQualified(filePath)) throw new IOException("文件位置必须是完整路径。");
            string fullPath = Path.GetFullPath(filePath);
            EnsureDirectChild(root, fullPath);
            FileAttributes attributes = File.GetAttributes(fullPath);
            if ((attributes & FileAttributes.Directory) != 0)
                return new(false, "", "文件夹仅显示名称，本轮不展开子目录。");
            if ((attributes & (FileAttributes.Hidden | FileAttributes.System | FileAttributes.ReparsePoint)) != 0)
                return new(false, "", "隐藏、系统或链接文件不支持预览。");
            string name = Path.GetFileName(fullPath);
            if (!TextExtensions.Contains(Path.GetExtension(name)) && !TextExtensions.Contains(name))
                return new(false, "", "此类型暂不支持预览。仅支持常见源码和文本文件；不会打开外部程序。");
            await using var stream = new FileStream(fullPath, FileMode.Open, FileAccess.Read, FileShare.Read,
                8192, FileOptions.Asynchronous | FileOptions.SequentialScan);
            // Verify the opened handle before reading. This also rejects a link introduced after the attribute check.
            VerifyOpenedPath(stream.SafeFileHandle, fullPath);
            if (stream.Length > MaximumTextBytes)
                return new(false, "", "文件超过 256 KiB，请在自己的编辑器中查看。");
            byte[] buffer = new byte[MaximumTextBytes + 1];
            int count = 0;
            while (count < buffer.Length)
            {
                int read = await stream.ReadAsync(buffer.AsMemory(count), cancellationToken).ConfigureAwait(false);
                if (read == 0) break;
                count += read;
            }
            if (count > MaximumTextBytes) return new(false, "", "文件超过 256 KiB，请在自己的编辑器中查看。");
            string text = DecodeText(buffer.AsSpan(0, count));
            if (text.Any(character => character == '\0' || (char.IsControl(character) && character is not '\r' and not '\n' and not '\t' and not '\f')))
                return new(false, "", "检测到二进制或非文本内容，无法预览。");
            return new(true, text, "只读预览 · 不会执行或上传文件内容");
        }
        catch (DecoderFallbackException) { return new(false, "", "文件不是有效的 UTF-8 或带标记的 UTF-16 文本。请在编辑器中转换编码后查看。"); }
        catch (Exception error) when (IsFileError(error)) { return new(false, "", "无法预览此文件：" + error.Message); }
    }

    private static string DecodeText(ReadOnlySpan<byte> bytes)
    {
        if (bytes.Length >= 4 && ((bytes[0] == 0 && bytes[1] == 0 && bytes[2] == 0xfe && bytes[3] == 0xff)
            || (bytes[0] == 0xff && bytes[1] == 0xfe && bytes[2] == 0 && bytes[3] == 0)))
            throw new DecoderFallbackException();
        if (bytes.StartsWith(new byte[] { 0xff, 0xfe })) return new UnicodeEncoding(false, true, true).GetString(bytes[2..]);
        if (bytes.StartsWith(new byte[] { 0xfe, 0xff })) return new UnicodeEncoding(true, true, true).GetString(bytes[2..]);
        if (bytes.StartsWith(new byte[] { 0xef, 0xbb, 0xbf })) bytes = bytes[3..];
        return new UTF8Encoding(false, true).GetString(bytes);
    }

    private static string ValidateRoot(string? rootPath)
    {
        if (string.IsNullOrWhiteSpace(rootPath)) throw new IOException("此工作没有关联文件夹。");
        if (!Path.IsPathFullyQualified(rootPath)) throw new IOException("关联目录必须是完整路径。");
        string root = Path.TrimEndingDirectorySeparator(Path.GetFullPath(rootPath));
        var directory = new DirectoryInfo(root);
        for (DirectoryInfo? current = directory; current is not null; current = current.Parent)
        {
            FileAttributes attributes = File.GetAttributes(current.FullName);
            if ((attributes & FileAttributes.ReparsePoint) != 0) throw new IOException("关联目录包含链接或重解析点，暂不支持读取。");
            if ((attributes & FileAttributes.Directory) == 0) throw new IOException("关联位置不是文件夹。");
        }
        return root;
    }

    private static void EnsureDirectChild(string root, string path)
    {
        string fullPath = Path.GetFullPath(path);
        string? parent = Path.GetDirectoryName(fullPath);
        if (parent is null || !string.Equals(Path.TrimEndingDirectorySeparator(parent), root, PathComparison))
            throw new IOException("文件不在所选目录的一级范围内。");
    }

    private static StringComparison PathComparison => OperatingSystem.IsWindows() ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal;
    private static bool IsFileError(Exception error) => error is IOException or UnauthorizedAccessException or ArgumentException or NotSupportedException or System.Security.SecurityException or Win32Exception;

    private static void VerifyOpenedPath(SafeFileHandle handle, string expectedPath)
    {
        if (!OperatingSystem.IsWindows()) return;
        var resolved = new StringBuilder(1024);
        uint length = GetFinalPathNameByHandle(handle, resolved, (uint)resolved.Capacity, 0);
        if (length == 0) throw new Win32Exception(Marshal.GetLastWin32Error());
        if (length >= resolved.Capacity)
        {
            resolved.EnsureCapacity(checked((int)length + 1));
            length = GetFinalPathNameByHandle(handle, resolved, (uint)resolved.Capacity, 0);
            if (length == 0 || length >= resolved.Capacity) throw new IOException("无法确认文件的实际位置。");
        }
        string actualPath = resolved.ToString();
        if (actualPath.StartsWith(@"\\?\UNC\", StringComparison.OrdinalIgnoreCase)) actualPath = @"\\" + actualPath[8..];
        else if (actualPath.StartsWith(@"\\?\", StringComparison.Ordinal)) actualPath = actualPath[4..];
        if (!string.Equals(Path.GetFullPath(actualPath), expectedPath, PathComparison))
            throw new IOException("文件的实际位置与所选文件不同，链接文件不支持预览。");
    }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern uint GetFinalPathNameByHandle(SafeFileHandle handle, StringBuilder path, uint pathLength, uint flags);
}
