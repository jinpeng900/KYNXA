using System.Security.Cryptography;
using System.Text;

namespace KYNXA.ToolHost;

/// <summary>Copies exactly the approved skill manifest; later changes never expand the sandbox input.</summary>
/// <remarks>只复制已批准清单中的技能文件，后续变更不能扩大沙箱输入范围。</remarks>
internal static class SkillSnapshot
{
    internal static void Copy(SandboxSkill request, string destination)
    {
        if (!Path.IsPathFullyQualified(request.Root) || request.Root.StartsWith("\\\\", StringComparison.Ordinal) ||
            request.Files.Length is < 1 or > 128)
            throw new SandboxException("SANDBOX_INVALID_SKILL", "Invalid skill manifest.");
        string root = Path.GetFullPath(request.Root);
        EnsureParents(root);
        long total = 0;
        var paths = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (SandboxSkillFile file in request.Files)
        {
            ValidateRelative(file.Path);
            if (!paths.Add(file.Path) || file.Size is < 0 or > 2 * 1024 * 1024 ||
                file.Sha256.Length != 64 || !file.Sha256.All(Uri.IsHexDigit) || (total += file.Size) > 8 * 1024 * 1024)
                throw new SandboxException("SANDBOX_INVALID_SKILL", "Skill manifest exceeds its bounds.");
            string source = Path.GetFullPath(Path.Combine(root, file.Path.Replace('/', Path.DirectorySeparatorChar)));
            if (!WorkspaceSnapshot.IsWithin(root, source)) throw new SandboxException("SANDBOX_INVALID_SKILL", "Skill path escapes its package.");
            EnsureParents(source);
            using var input = new FileStream(source, FileMode.Open, FileAccess.Read, FileShare.Read);
            NativeMethods.Check(NativeMethods.GetFileInformationByHandle(input.SafeFileHandle, out var information), "GetFileInformation skill");
            if (information.NumberOfLinks != 1 || input.Length != file.Size)
                throw new SandboxException("APP_SKILL_CHANGED", "Skill file changed or became linked after approval.");
            var finalPath = new StringBuilder(32768);
            uint finalLength = NativeMethods.GetFinalPathNameByHandleW(input.SafeFileHandle, finalPath, (uint)finalPath.Capacity, 0);
            NativeMethods.Check(finalLength > 0 && finalLength < finalPath.Capacity, "GetFinalPathName skill");
            string resolved = finalPath.ToString();
            if (resolved.StartsWith("\\\\?\\", StringComparison.Ordinal)) resolved = resolved[4..];
            if (!WorkspaceSnapshot.IsWithin(root, resolved)) throw new SandboxException("SANDBOX_INVALID_SKILL", "Opened skill file resolves outside its package.");
            string target = Path.Combine(destination, file.Path.Replace('/', Path.DirectorySeparatorChar));
            Directory.CreateDirectory(Path.GetDirectoryName(target)!);
            using (var output = new FileStream(target, FileMode.CreateNew, FileAccess.Write, FileShare.None)) input.CopyTo(output);
            using var copied = File.OpenRead(target);
            string hash = Convert.ToHexString(SHA256.HashData(copied));
            if (!hash.Equals(file.Sha256, StringComparison.OrdinalIgnoreCase))
                throw new SandboxException("APP_SKILL_CHANGED", "Skill contents changed after approval; nothing was executed.");
        }
        ValidateRelative(request.Script);
        if (!paths.Contains(request.Script) || Path.GetExtension(request.Script).ToLowerInvariant() is not (".js" or ".mjs" or ".cjs"))
            throw new SandboxException("APP_SKILL_SCRIPT_UNSUPPORTED", "The selected script must be a manifest-backed Node.js file.");
    }

    private static void ValidateRelative(string path)
    {
        if (string.IsNullOrWhiteSpace(path) || path.Length > 2048 || path.Contains('\\') || Path.IsPathRooted(path) ||
            path.Split('/').Any(part => part is "" or "." or ".." || part.EndsWith('.') || part.EndsWith(' ') ||
                part.IndexOfAny(Path.GetInvalidFileNameChars()) >= 0 || WorkspaceSnapshot.IsSensitiveFile(part) ||
                new[] { "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9" }.Contains(part.Split('.')[0], StringComparer.OrdinalIgnoreCase)))
            throw new SandboxException("SANDBOX_INVALID_SKILL", "Skill paths must be safe package-relative filenames.");
    }

    private static void EnsureParents(string path)
    {
        for (string? current = path; current is not null; current = Path.GetDirectoryName(current))
            if ((File.GetAttributes(current) & FileAttributes.ReparsePoint) != 0)
                throw new SandboxException("SANDBOX_INVALID_SKILL", "Skill files and ancestors cannot be linked.");
    }
}

internal sealed record SandboxSkill
{
    public string Root { get; init; } = "";
    public string Script { get; init; } = "";
    public SandboxSkillFile[] Files { get; init; } = [];
}

internal sealed record SandboxSkillFile
{
    public string Path { get; init; } = "";
    public long Size { get; init; }
    public string Sha256 { get; init; } = "";
}
