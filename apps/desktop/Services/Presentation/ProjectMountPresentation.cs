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
        if (project is null || project.IsArchived || project.IsFolderlessWorkspace ||
            string.IsNullOrWhiteSpace(project.FolderPath) || !Path.IsPathFullyQualified(project.FolderPath)) return null;
        string folder = Path.TrimEndingDirectorySeparator(Path.GetFullPath(project.FolderPath));
        string managedParent = Path.Combine(desktopDirectory, "Projects");
        if (string.Equals(Path.GetDirectoryName(folder), managedParent, StringComparison.OrdinalIgnoreCase) &&
            Guid.TryParse(Path.GetFileName(folder), out Guid folderId) && folderId == project.Id) return null;
        return folder;
    }
}
