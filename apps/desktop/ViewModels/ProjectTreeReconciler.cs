using System.Collections.ObjectModel;
using KYNXA_Desktop.Models.UI;

namespace KYNXA_Desktop.ViewModels;

/// <summary>
/// Updates the sidebar in place, preserving data-row identity and unaffected native containers.
/// 原地更新侧栏，保留数据行身份与未受影响的原生容器。
/// </summary>
public static class ProjectTreeReconciler
{
    public static void Update(ObservableCollection<ProjectTreeEntry> entries,
        IEnumerable<ProjectState> projects, Guid? activeChatId,
        IReadOnlySet<Guid> collapsedByUser, IReadOnlySet<Guid> expandProjectIds,
        Guid? selectedProjectId = null)
    {
        var existing = entries.ToDictionary(entry => entry.Project.Id);
        var desired = new List<ProjectTreeEntry>();
        foreach (var project in projects.Where(project => !project.IsArchived && !project.IsFolderlessWorkspace)
                     .OrderByDescending(project => project.IsPinned))
        {
            if (!existing.TryGetValue(project.Id, out var entry)) entry = new ProjectTreeEntry(project);
            entry.Refresh(project);
            entry.IsActive = project.Id == selectedProjectId;

            var existingChats = entry.Children.ToDictionary(child => child.Chat!.Id);
            var desiredChats = new List<ProjectTreeEntry>();
            foreach (var chat in project.Chats.Where(chat => chat.CanPersist && !chat.IsArchived).OrderByDescending(chat => chat.IsPinned))
            {
                if (!existingChats.TryGetValue(chat.Id, out var child)) child = new ProjectTreeEntry(project, chat);
                child.Refresh(project, chat);
                child.IsActive = chat.Id == activeChatId;
                desiredChats.Add(child);
            }
            ReconcileRows(entry.Children, desiredChats);
            entry.IsExpanded = !collapsedByUser.Contains(project.Id) &&
                (entry.IsExpanded || expandProjectIds.Contains(project.Id));
            desired.Add(entry);
        }
        ReconcileRows(entries, desired);
    }

    internal static void ReconcileRows(ObservableCollection<ProjectTreeEntry> rows, IReadOnlyList<ProjectTreeEntry> desired)
    {
        var retained = desired.ToHashSet();
        for (int index = rows.Count - 1; index >= 0; index--)
            if (!retained.Contains(rows[index])) rows.RemoveAt(index);

        for (int index = 0; index < desired.Count; index++)
        {
            var entry = desired[index];
            if (index < rows.Count && ReferenceEquals(rows[index], entry)) continue;
            int previous = rows.IndexOf(entry);
            if (previous < 0) rows.Insert(index, entry);
            else
            {
                // TreeView's WinRT ItemsSource projection does not reliably process
                // ObservableCollection.Move. Express the move as two supported changes;
                // the row object and its expansion state remain the same.
                // TreeView 的 WinRT ItemsSource 投影不能可靠处理 ObservableCollection.Move；用受支持的移除与插入表达移动，行对象和展开状态不变。
                rows.RemoveAt(previous);
                rows.Insert(index, entry);
            }
        }
    }
}
