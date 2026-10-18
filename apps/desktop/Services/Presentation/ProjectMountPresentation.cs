using KYNXA_Desktop.Models.UI;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Pure display projection: legacy app-managed directories are not external mounts.
/// 纯展示投影；旧版应用托管目录不算用户挂载的外部文件夹。
/// </summary>
public static class ProjectMountPresentation
{
    public static string? UserFolder(ProjectState? project, string desktopDirectory)
    {
        if (project is null || project.IsArchived || project.IsFolderlessWorkspace) return null;
        string? path = project.Mounts.FirstOrDefault(mount => mount.Id == project.DefaultMountId)?.Path ?? project.FolderPath;
        if (string.IsNullOrWhiteSpace(path) || !Path.IsPathFullyQualified(path)) return null;
        string folder = Path.TrimEndingDirectorySeparator(Path.GetFullPath(path));
        string managedParent = Path.Combine(desktopDirectory, "Projects");
        if (string.Equals(Path.GetDirectoryName(folder), managedParent, StringComparison.OrdinalIgnoreCase) &&
            Guid.TryParse(Path.GetFileName(folder), out Guid folderId) && folderId == project.Id) return null;
        return folder;
    }
}
