using System.Collections.ObjectModel;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.ViewModels;

namespace KYNXA_Desktop.Services;

/// <summary>Views over work chats; sidebar navigation never changes their storage ownership.</summary>
public static class WorkSidebarState
{
    public static ProjectState? FindSelectedProject(IEnumerable<ProjectState> projects, Guid? projectId) =>
        projectId is Guid id
            ? projects.FirstOrDefault(project => project.Id == id && !project.IsArchived && !project.IsFolderlessWorkspace)
            : null;

    public static IEnumerable<(ProjectState Project, ProjectChatState Chat)> RecentChats(
        IEnumerable<ProjectState> projects, IReadOnlyList<Guid>? recentOrder = null)
    {
        var ranks = new Dictionary<Guid, int>();
        if (recentOrder is not null)
            for (int index = 0; index < recentOrder.Count; index++) ranks.TryAdd(recentOrder[index], index);
        return projects.Where(project => !project.IsArchived)
            .SelectMany(project => ProjectChats(project))
            .OrderByDescending(entry => entry.Chat.IsPinned)
            .ThenBy(entry => !entry.Chat.IsPinned && ranks.TryGetValue(entry.Chat.Id, out int rank) ? rank : int.MaxValue);
    }

    /// <summary>Tasks may show the selected project's active draft. Other views omit drafts by default.</summary>
    public static IEnumerable<(ProjectState Project, ProjectChatState Chat)> ProjectChats(
        ProjectState? project, Guid? activeDraftId = null) =>
        project is null || project.IsArchived
            ? []
            : project.Chats.Where(chat => !chat.IsArchived &&
                (chat.CanPersist || (!project.IsFolderlessWorkspace && chat.Id == activeDraftId)))
                .OrderByDescending(chat => chat.IsPinned)
                .Select(chat => (project, chat));

    /// <summary>Call when submitting a message, never on navigation or streaming updates. Pinned rows retain their order.</summary>
    public static bool ActivateChat(ProjectState project, ProjectChatState chat)
    {
        if (project.IsArchived || chat.IsPinned || chat.IsArchived || !chat.CanPersist) return false;
        int index = project.Chats.IndexOf(chat);
        if (index <= 0) return false;
        project.Chats.RemoveAt(index);
        project.Chats.Insert(0, chat);
        return true;
    }

    public static void ReconcileChats(ObservableCollection<ProjectTreeEntry> rows,
        IEnumerable<(ProjectState Project, ProjectChatState Chat)> chats,
        Guid? activeChatId, IReadOnlySet<Guid> replyingIds)
    {
        var existing = rows.ToDictionary(entry => (entry.Project.Id, entry.Chat!.Id));
        var desired = new List<ProjectTreeEntry>();
        foreach (var (project, chat) in chats)
        {
            if (!existing.TryGetValue((project.Id, chat.Id), out var entry)) entry = new ProjectTreeEntry(project, chat);
            entry.Refresh(project, chat);
            entry.IsActive = chat.Id == activeChatId;
            entry.IsReplying = replyingIds.Contains(chat.Id);
            desired.Add(entry);
        }
        // Avoid resets and retain row identity so hover, focus and selection remain stable.
        ProjectTreeReconciler.ReconcileRows(rows, desired);
    }
}
