using System.Collections.ObjectModel;
using System.Collections.Specialized;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;

int checks = 0;
void Check(bool condition, string description)
{
    if (!condition) throw new InvalidOperationException(description);
    checks++;
}
ProjectChatState Chat(string title, bool pinned = false) => new()
{
    Title = title, IsPinned = pinned, Messages = [new() { Content = title }]
};

var older = Chat("Older");
var pinned = Chat("Pinned", true);
var newer = Chat("Newer");
var draft = new ProjectChatState { Title = "Unsent", Draft = "Not a message" };
var archived = Chat("Archived");
archived.IsArchived = true;
var project = new ProjectState { Name = "Project", Chats = [older, pinned, newer, draft, archived] };
var recentOlder = Chat("Recent older");
var recentPinned = Chat("Recent pinned", true);
var recentNewer = Chat("Recent newer");
var folderless = new ProjectState
{
    Name = "Without folder", IsFolderlessWorkspace = true, Chats = [recentOlder, recentPinned, recentNewer, draft, archived]
};
var unavailable = new ProjectState { Name = "Archived project", IsArchived = true, Chats = [Chat("Hidden")] };
var projects = new List<ProjectState> { folderless, project, unavailable };

Check(WorkSidebarState.FindSelectedProject(projects, project.Id) == project,
    "A project without a folder path is still a project, not a folderless chat bucket.");
Check(WorkSidebarState.FindSelectedProject(projects, null) is null &&
      WorkSidebarState.FindSelectedProject(projects, Guid.NewGuid()) is null &&
      WorkSidebarState.FindSelectedProject(projects, folderless.Id) is null &&
      WorkSidebarState.FindSelectedProject(projects, unavailable.Id) is null,
    "Tasks cannot select absent, archived, or folderless workspace projects.");
Check(WorkSidebarState.ProjectChats(null).Count() == 0 && WorkSidebarState.ProjectChats(unavailable).Count() == 0,
    "No selected project leaves task data empty.");
Check(WorkSidebarState.RecentChats(projects).Select(entry => entry.Chat).SequenceEqual([recentPinned, pinned, recentOlder, recentNewer, older, newer]),
    "Recent work includes every project's saved chats while excluding empty drafts and archived chats, with global pins first.");
Check(WorkSidebarState.RecentChats(projects, [newer.Id, recentNewer.Id, older.Id, recentOlder.Id])
        .Select(entry => entry.Chat).SequenceEqual([recentPinned, pinned, newer, recentNewer, older, recentOlder]),
    "MRU order moves unpinned chats across project boundaries without disturbing global pins.");
Check(WorkSidebarState.RecentChats(projects, [Guid.NewGuid(), older.Id, older.Id, pinned.Id, archived.Id])
        .Select(entry => entry.Chat).SequenceEqual([recentPinned, pinned, older, recentOlder, recentNewer, newer]),
    "Unknown, duplicate, archived and pinned MRU IDs do not corrupt fallback order or pinned positions.");
Check(WorkSidebarState.RecentChats(projects, []).Select(entry => entry.Chat)
        .SequenceEqual(WorkSidebarState.RecentChats(projects).Select(entry => entry.Chat)),
    "An absent or empty MRU index uses stable project and chat order.");
Check(WorkSidebarState.ProjectChats(project).Select(entry => entry.Chat).SequenceEqual([pinned, older, newer]),
    "Tasks contain all saved, unarchived project chats, not only replies that are still generating.");
Check(!WorkSidebarState.ActivateChat(project, pinned) && !WorkSidebarState.ActivateChat(project, draft) &&
      !WorkSidebarState.ActivateChat(project, archived) && !WorkSidebarState.ActivateChat(project, recentNewer) &&
      !WorkSidebarState.ActivateChat(unavailable, unavailable.Chats[0]),
    "Activation ignores pins, drafts, archived records and chats belonging to another project.");
Check(WorkSidebarState.ActivateChat(project, newer) && !WorkSidebarState.ActivateChat(project, newer) &&
      WorkSidebarState.ProjectChats(project).Select(entry => entry.Chat).SequenceEqual([pinned, newer, older]),
    "Submitting a saved chat advances it once beneath pins and retains the order of other chats.");
Check(WorkSidebarState.ActivateChat(folderless, recentNewer) &&
      WorkSidebarState.RecentChats(projects).Select(entry => entry.Chat).SequenceEqual([recentPinned, pinned, recentNewer, recentOlder, newer, older]),
    "Folderless work gets the same fallback recent ordering as project tasks.");

var rows = new ObservableCollection<ProjectTreeEntry>();
var replying = new HashSet<Guid> { newer.Id };
int mutations = 0, resets = 0;
rows.CollectionChanged += (_, args) =>
{
    mutations++;
    if (args.Action == NotifyCollectionChangedAction.Reset) resets++;
};
WorkSidebarState.ReconcileChats(rows, WorkSidebarState.ProjectChats(project), newer.Id, replying);
var newerRow = rows.Single(row => row.Chat?.Id == newer.Id);
var pinnedRow = rows.Single(row => row.Chat?.Id == pinned.Id);
Check(newerRow.IsActive && newerRow.ActiveVisibility == Visibility.Visible &&
      newerRow.IsReplying && newerRow.ReplyingVisibility == Visibility.Visible,
    "Active and generating chat states are independent view properties.");
Check(pinnedRow.Glyph == "\uE718" && newerRow.Glyph != "\uE718", "Chat pin icons use chat pin state.");
var notifications = new List<string>();
newerRow.PropertyChanged += (_, args) => notifications.Add(args.PropertyName ?? "");
int before = mutations;
WorkSidebarState.ReconcileChats(rows, WorkSidebarState.ProjectChats(project), newer.Id, replying);
Check(before == mutations && newerRow == rows.Single(row => row.Chat?.Id == newer.Id),
    "An unchanged refresh makes no collection mutations and preserves row identity.");
replying.Clear();
WorkSidebarState.ReconcileChats(rows, WorkSidebarState.ProjectChats(project), null, replying);
Check(!newerRow.IsReplying && !newerRow.IsActive && notifications.Contains(nameof(ProjectTreeEntry.ReplyingVisibility)) &&
      notifications.Contains(nameof(ProjectTreeEntry.ActiveVisibility)),
    "Ending a reply or switching away updates binding visibility without replacing a row.");
older.IsPinned = true;
older.Title = "Renamed older";
WorkSidebarState.ReconcileChats(rows, WorkSidebarState.ProjectChats(project), null, replying);
Check(rows[0].Chat == older && rows.Single(row => row.Chat == pinned) == pinnedRow &&
      rows[0].Title == "Renamed older" && rows[0].Glyph == "\uE718",
    "Pinning and renaming change order and metadata while retaining unaffected rows.");
var reloadedChat = new ProjectChatState { Id = newer.Id, Title = "Reloaded", Messages = newer.Messages };
var reloadedProject = new ProjectState { Id = project.Id, Name = "Reloaded project", Chats = [reloadedChat] };
WorkSidebarState.ReconcileChats(rows, WorkSidebarState.ProjectChats(reloadedProject), reloadedChat.Id, replying);
Check(rows.Count == 1 && rows[0] == newerRow && newerRow.Chat == reloadedChat && newerRow.Project == reloadedProject &&
      newerRow.Title == "Reloaded", "Catalog reload refreshes references in existing rows.");
WorkSidebarState.ReconcileChats(rows, [], null, replying);
Check(rows.Count == 0 && resets == 0, "Changing task scope removes obsolete rows without any collection reset.");

var tree = new ObservableCollection<ProjectTreeEntry>();
var collapsed = new HashSet<Guid>();
var expand = new HashSet<Guid>();
ProjectTreeReconciler.Update(tree, projects, newer.Id, collapsed, expand, project.Id);
var root = tree.Single();
Check(root.Project == project && root.IsActive && root.Children.All(row => row.Chat!.CanPersist && !row.Chat.IsArchived),
    "Tree root selection is separate from chat selection and excludes unsaved children.");
Check(!root.IsExpanded && root.Children.Single(row => row.Chat == newer).IsActive,
    "Opening a chat never expands its project automatically.");
expand.Add(project.Id);
ProjectTreeReconciler.Update(tree, projects, newer.Id, collapsed, expand, project.Id);
Check(root.IsExpanded, "An explicit user expansion can open a project.");
expand.Clear();
ProjectTreeReconciler.Update(tree, projects, older.Id, collapsed, expand);
Check(root.IsExpanded && !root.IsActive, "Refresh preserves user expansion independently of selected project and active chat.");
collapsed.Add(project.Id);
expand.Add(project.Id);
ProjectTreeReconciler.Update(tree, projects, newer.Id, collapsed, expand, project.Id);
Check(!root.IsExpanded, "A user's collapse remains authoritative over a stale expansion request.");
collapsed.Clear();
expand.Clear();
ProjectTreeReconciler.Update(tree, projects, newer.Id, collapsed, expand, project.Id);
Check(!root.IsExpanded, "Removing collapse bookkeeping does not automatically reopen a project.");

// Exercise the production view functions using an existing manual project/chat order.
// Navigation changes scope and highlights; only the explicit submission services reorder.
var firstChat = Chat("First project's first chat");
var firstPinnedChat = Chat("First project's pinned chat", true);
var firstOtherChat = Chat("First project's other chat");
var firstProject = new ProjectState { Name = "First", Chats = [firstChat, firstPinnedChat, firstOtherChat] };
var targetOlderChat = Chat("Target's older chat");
var targetPinnedChat = Chat("Target's pinned chat", true);
var targetChat = Chat("Target's opened chat");
var targetProject = new ProjectState { Name = "Target", Chats = [targetOlderChat, targetPinnedChat, targetChat] };
var pinnedProject = new ProjectState { Name = "Pinned project", IsPinned = true, Chats = [Chat("Pinned project's chat")] };
var navigationProjects = new List<ProjectState> { firstProject, pinnedProject, targetProject };
var recentOrder = new List<Guid> { firstOtherChat.Id, targetOlderChat.Id, firstChat.Id, targetChat.Id };
var sourceProjectIds = navigationProjects.Select(value => value.Id).ToArray();
var sourceChatIds = navigationProjects.ToDictionary(value => value.Id, value => value.Chats.Select(chat => chat.Id).ToArray());
var recentIds = WorkSidebarState.RecentChats(navigationProjects, recentOrder).Select(value => value.Chat.Id).ToArray();
var navigationTree = new ObservableCollection<ProjectTreeEntry>();
var navigationRecent = new ObservableCollection<ProjectTreeEntry>();
var navigationTasks = new ObservableCollection<ProjectTreeEntry>();
var noReplies = new HashSet<Guid>();
var userCollapsed = new HashSet<Guid>();
var userExpanded = new HashSet<Guid> { firstProject.Id };
ProjectTreeReconciler.Update(navigationTree, navigationProjects, firstChat.Id, userCollapsed, userExpanded, firstProject.Id);
userExpanded.Clear();
WorkSidebarState.ReconcileChats(navigationRecent, WorkSidebarState.RecentChats(navigationProjects, recentOrder), firstChat.Id, noReplies);
var recentContainers = navigationRecent.ToDictionary(value => value.Chat!.Id);
foreach (var (selected, activeChat) in new (ProjectState?, ProjectChatState?)[]
    { (targetProject, targetChat), (firstProject, firstOtherChat), (targetProject, null), (null, null), (targetProject, targetChat) })
{
    var selectedProject = WorkSidebarState.FindSelectedProject(navigationProjects, selected?.Id);
    ProjectTreeReconciler.Update(navigationTree, navigationProjects, activeChat?.Id, userCollapsed, userExpanded, selectedProject?.Id);
    WorkSidebarState.ReconcileChats(navigationRecent, WorkSidebarState.RecentChats(navigationProjects, recentOrder), activeChat?.Id, noReplies);
    WorkSidebarState.ReconcileChats(navigationTasks, WorkSidebarState.ProjectChats(selectedProject), activeChat?.Id, noReplies);
    Check(navigationProjects.Select(value => value.Id).SequenceEqual(sourceProjectIds) &&
          navigationProjects.All(value => value.Chats.Select(chat => chat.Id).SequenceEqual(sourceChatIds[value.Id])) &&
          navigationRecent.Select(value => value.Chat!.Id).SequenceEqual(recentIds),
        "Opening another chat or selecting its workspace leaves the project, chat and recent histories in their existing order.");
    Check(navigationTasks.All(value => value.Project == selectedProject) &&
          navigationTasks.Select(value => value.Chat!.Id).SequenceEqual(WorkSidebarState.ProjectChats(selectedProject).Select(value => value.Chat.Id)) &&
          navigationTree.Where(value => value.IsActive).Select(value => value.Project.Id)
              .SequenceEqual(selectedProject is null ? [] : new[] { selectedProject.Id }),
        "Navigation changes the task scope and selected project together without moving any history entry.");
}
Check(navigationTree.Single(value => value.Project == firstProject).IsExpanded &&
      !navigationTree.Single(value => value.Project == targetProject).IsExpanded &&
      navigationRecent.All(value => value == recentContainers[value.Chat!.Id]),
    "Repeated navigation preserves manual project expansion and every recent row identity.");

Check(ProjectOrdering.Activate(navigationProjects, targetProject) && WorkSidebarState.ActivateChat(targetProject, targetChat),
    "A submitted message can promote its saved project and chat through the production ordering services.");
ProjectTreeReconciler.Update(navigationTree, navigationProjects, targetChat.Id, userCollapsed, userExpanded, targetProject.Id);
WorkSidebarState.ReconcileChats(navigationTasks, WorkSidebarState.ProjectChats(targetProject), targetChat.Id, noReplies);
Check(navigationTree.Select(value => value.Project).SequenceEqual([pinnedProject, targetProject, firstProject]) &&
      navigationTasks.Select(value => value.Chat).SequenceEqual([targetPinnedChat, targetChat, targetOlderChat]),
    "Submission places the active project and chat immediately below their respective pinned groups.");
Check(!ProjectOrdering.Activate(navigationProjects, pinnedProject) && !WorkSidebarState.ActivateChat(targetProject, targetPinnedChat),
    "Submission never promotes pinned projects or chats ahead of other pins.");
var afterSubmissionProjects = navigationProjects.Select(value => value.Id).ToArray();
var afterSubmissionChats = targetProject.Chats.Select(value => value.Id).ToArray();
ProjectTreeReconciler.Update(navigationTree, navigationProjects, targetOlderChat.Id, userCollapsed, userExpanded, targetProject.Id);
WorkSidebarState.ReconcileChats(navigationTasks, WorkSidebarState.ProjectChats(targetProject), targetOlderChat.Id, noReplies);
Check(navigationProjects.Select(value => value.Id).SequenceEqual(afterSubmissionProjects) &&
      targetProject.Chats.Select(value => value.Id).SequenceEqual(afterSubmissionChats) &&
      navigationTasks.Single(value => value.Chat == targetOlderChat).IsActive &&
      !navigationTree.Single(value => value.Project == targetProject).IsExpanded,
    "Viewing an older chat after submission selects it without undoing the submitted order or opening its tree.");

Console.WriteLine($"PASS: {checks} work sidebar state checks; no native window or user data accessed.");
