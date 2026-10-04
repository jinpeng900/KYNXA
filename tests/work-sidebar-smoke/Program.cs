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
// 用已有的手动项目和聊天顺序验证生产视图；导航只改变范围和高亮，仅显式发送流程调整排序。
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

// A task-only draft remains transient until an actual message is submitted.
// 仅任务中的草稿在实际发送消息前保持临时状态。
var draftSavedOlder = Chat("Draft project's older chat");
var draftSavedPinned = Chat("Draft project's pinned chat", true);
var draftSavedNewer = Chat("Draft project's newer chat");
var activeDraft = new ProjectChatState();
var inactiveDraft = new ProjectChatState { Draft = "Typed, but not submitted" };
var archivedDraft = new ProjectChatState { IsArchived = true };
var draftProject = new ProjectState
{
    Name = "Draft project", Chats = [draftSavedOlder, draftSavedPinned, draftSavedNewer, activeDraft, inactiveDraft, archivedDraft]
};
var otherSavedChat = Chat("Other project's saved chat");
var otherDraft = new ProjectChatState();
var otherDraftProject = new ProjectState { Name = "Other draft project", Chats = [otherSavedChat, otherDraft] };
var draftFolderless = new ProjectState
{
    Name = "Folderless drafts", IsFolderlessWorkspace = true, Chats = [Chat("Folderless saved"), new()]
};
var archivedDraftProject = new ProjectState { IsArchived = true, Chats = [new()] };
var draftProjects = new List<ProjectState> { otherDraftProject, draftProject, draftFolderless, archivedDraftProject };
var draftProjectOrder = draftProjects.Select(value => value.Id).ToArray();
var draftChatOrder = draftProject.Chats.Select(value => value.Id).ToArray();
var draftRecentOrder = new List<Guid> { draftSavedNewer.Id, otherSavedChat.Id, draftSavedOlder.Id };
var draftRecentIds = WorkSidebarState.RecentChats(draftProjects, draftRecentOrder).Select(value => value.Chat.Id).ToArray();

Check(!activeDraft.CanPersist && activeDraft.Title == "新聊天" &&
      WorkSidebarState.ProjectChats(draftProject, activeDraft.Id).Select(value => value.Chat)
          .SequenceEqual([draftSavedPinned, draftSavedOlder, draftSavedNewer, activeDraft]),
    "Tasks immediately include only the selected empty new chat together with saved project chats, without making it persistent.");
Check(WorkSidebarState.ProjectChats(draftProject).All(value => value.Chat.CanPersist) &&
      WorkSidebarState.ProjectChats(draftProject, Guid.NewGuid()).All(value => value.Chat.CanPersist) &&
      WorkSidebarState.ProjectChats(draftProject, otherDraft.Id).All(value => value.Chat.CanPersist),
    "Default views, unknown IDs and another project's draft ID cannot expose an empty chat.");
Check(WorkSidebarState.ProjectChats(draftProject, inactiveDraft.Id).Select(value => value.Chat)
          .SequenceEqual([draftSavedPinned, draftSavedOlder, draftSavedNewer, inactiveDraft]) && !inactiveDraft.CanPersist,
    "A newly selected draft replaces the previous task-only draft, and unsent input still does not become history.");
Check(WorkSidebarState.ProjectChats(draftProject, archivedDraft.Id).All(value => value.Chat.CanPersist) &&
      !WorkSidebarState.ProjectChats(draftFolderless, draftFolderless.Chats[1].Id).Any(value => !value.Chat.CanPersist) &&
      !WorkSidebarState.ProjectChats(archivedDraftProject, archivedDraftProject.Chats[0].Id).Any() &&
      !WorkSidebarState.ProjectChats(null, activeDraft.Id).Any(),
    "Archived chats, archived projects, folderless work and no selected project cannot expose task drafts.");
Check(WorkSidebarState.RecentChats(draftProjects, [activeDraft.Id, .. draftRecentOrder])
          .Select(value => value.Chat.Id).SequenceEqual(draftRecentIds),
    "An active task draft stays out of Recent even when its ID appears in the recent order.");
var draftTree = new ObservableCollection<ProjectTreeEntry>();
ProjectTreeReconciler.Update(draftTree, draftProjects, activeDraft.Id, new HashSet<Guid>(), new HashSet<Guid>(), draftProject.Id);
Check(draftTree.Single(value => value.Project == draftProject).Children.All(value => value.Chat!.CanPersist) &&
      !draftTree.Single(value => value.Project == draftProject).IsExpanded,
    "The project tree continues to exclude every empty chat and does not expand for a task draft.");
Check(draftProjects.Select(value => value.Id).SequenceEqual(draftProjectOrder) &&
      draftProject.Chats.Select(value => value.Id).SequenceEqual(draftChatOrder) &&
      draftRecentOrder.SequenceEqual([draftSavedNewer.Id, otherSavedChat.Id, draftSavedOlder.Id]) &&
      !WorkSidebarState.ActivateChat(draftProject, activeDraft),
    "Creating or viewing a draft never promotes its project, chat or Recent position, and submission ordering rejects an empty draft.");

var draftTasks = new ObservableCollection<ProjectTreeEntry>();
WorkSidebarState.ReconcileChats(draftTasks, WorkSidebarState.ProjectChats(draftProject, activeDraft.Id), activeDraft.Id, noReplies);
var activeDraftRow = draftTasks.Single(value => value.Chat == activeDraft);
Check(activeDraftRow.Title == "新聊天" && activeDraftRow.IsActive,
    "The new task row displays its temporary title and selection immediately.");
WorkSidebarState.ReconcileChats(draftTasks, WorkSidebarState.ProjectChats(otherDraftProject, activeDraft.Id), null, noReplies);
Check(draftTasks.Count == 1 && draftTasks[0].Chat == otherSavedChat &&
      draftTasks.All(value => value.Project == otherDraftProject),
    "Changing the selected project removes the previous task draft and does not reveal the other project's inactive draft.");
WorkSidebarState.ReconcileChats(draftTasks, WorkSidebarState.ProjectChats(draftProject), null, noReplies);
Check(draftTasks.All(value => value.Chat!.CanPersist) && !draftTasks.Contains(activeDraftRow),
    "Returning without the active draft ID does not restore a transient task row.");
draftProject.Chats.Remove(activeDraft);
Check(!WorkSidebarState.ProjectChats(draftProject, activeDraft.Id).Any(value => value.Chat == activeDraft),
    "A draft discarded by navigation cannot be shown again through a stale active ID.");

var submittedDraft = new ProjectChatState();
draftProject.Chats.Add(submittedDraft);
WorkSidebarState.ReconcileChats(draftTasks, WorkSidebarState.ProjectChats(draftProject, submittedDraft.Id), submittedDraft.Id, noReplies);
var submittedDraftRow = draftTasks.Single(value => value.Chat == submittedDraft);
var beforeDraftSubmitOrder = draftProject.Chats.Select(value => value.Id).ToArray();
submittedDraft.Messages.Add(new() { Content = "The first submitted message" });
Check(submittedDraft.CanPersist && WorkSidebarState.ProjectChats(draftProject).Any(value => value.Chat == submittedDraft) &&
      draftProject.Chats.Select(value => value.Id).SequenceEqual(beforeDraftSubmitOrder),
    "The first real message makes the chat permanent without a view query independently changing its order.");
Check(ProjectOrdering.Activate(draftProjects, draftProject) && WorkSidebarState.ActivateChat(draftProject, submittedDraft),
    "Only an explicit submitted message can promote the new project chat through the existing ordering services.");
draftRecentOrder.Remove(submittedDraft.Id);
draftRecentOrder.Insert(0, submittedDraft.Id);
WorkSidebarState.ReconcileChats(draftTasks, WorkSidebarState.ProjectChats(draftProject), submittedDraft.Id, noReplies);
ProjectTreeReconciler.Update(draftTree, draftProjects, submittedDraft.Id, new HashSet<Guid>(), new HashSet<Guid>(), draftProject.Id);
Check(draftTasks.Select(value => value.Chat).SequenceEqual([draftSavedPinned, submittedDraft, draftSavedOlder, draftSavedNewer]) &&
      draftTasks.Single(value => value.Chat == submittedDraft) == submittedDraftRow && submittedDraftRow.IsActive,
    "A sent draft remains visible beneath pins with the same task row identity and active selection.");
Check(draftProjects[0] == draftProject &&
      WorkSidebarState.RecentChats(draftProjects, draftRecentOrder).Select(value => value.Chat)
          .Take(2).SequenceEqual([draftSavedPinned, submittedDraft]) &&
      draftTree.Single(value => value.Project == draftProject).Children.Any(value => value.Chat == submittedDraft),
    "After submission the permanent chat appears in Recent and the project tree, and explicit ordering puts it below pins.");
WorkSidebarState.ReconcileChats(draftTasks, WorkSidebarState.ProjectChats(otherDraftProject), otherSavedChat.Id, noReplies);
WorkSidebarState.ReconcileChats(draftTasks, WorkSidebarState.ProjectChats(draftProject), null, noReplies);
Check(draftTasks.Any(value => value.Chat == submittedDraft) &&
      draftTasks.All(value => value.Chat!.CanPersist),
    "Leaving and returning after submission keeps the permanent chat while all other empty drafts remain excluded.");

Console.WriteLine($"PASS: {checks} work sidebar state checks; no native window or user data accessed.");
