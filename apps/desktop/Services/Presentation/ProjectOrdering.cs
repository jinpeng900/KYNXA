using KYNXA_Desktop.Models.UI;

namespace KYNXA_Desktop.Services;

public static class ProjectOrdering
{
    public static bool Activate(List<ProjectState> projects, ProjectState project)
    {
        if (project.IsPinned || project.IsArchived || project.IsFolderlessWorkspace) return false;
        int index = projects.IndexOf(project);
        if (index <= 0) return false;
        projects.RemoveAt(index);
        projects.Insert(0, project); // Display always groups pinned projects first.
        return true;
    }

    public static bool Move(List<ProjectState> projects, ProjectState source, ProjectState target, bool after)
    {
        if (source == target || source.IsPinned != target.IsPinned || !projects.Contains(source) || !projects.Contains(target)) return false;
        projects.Remove(source);
        projects.Insert(projects.IndexOf(target) + (after ? 1 : 0), source);
        return true;
    }
}
