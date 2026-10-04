using System.Collections.ObjectModel;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop.Views;

// Minimal host for the production tree renderer and hover callbacks. This fixture
// never creates storage clients, loads user settings, or opens real conversations.
public sealed partial class ShellPage : Page
{
    public ObservableCollection<ProjectTreeEntry> Entries { get; } = [];
    public ObservableCollection<ProjectTreeEntry> ProjectEntries => Entries;
    public TreeView ProjectTree => Tree;
    private List<ProjectState> _projects = [];
    private ProjectChatState? _activeProjectChat;
    private Guid? _selectedWorkProjectId;
    private readonly Dictionary<Guid, object> _pendingReplies = [];
    private bool _projectViewClosed;
    public int RenderPasses => _projectRenderVersion;
    public bool IsRendering => _renderingProjects;
    public ShellPage() => InitializeComponent();
    private void RebuildWorkTasks() { }
    private static readonly string FixtureDesktopDirectory = Path.Combine(Path.GetTempPath(), "kynxa-tree-fixture", "Desktop");
    public string? MountedFolder { get; private set; }
    private static string? UserMountedFolder(ProjectState? project) =>
        KYNXA_Desktop.Services.ProjectMountPresentation.UserFolder(project, FixtureDesktopDirectory);
    private void UpdateMountedWorkspacePresentation() => MountedFolder = UserMountedFolder(
        _projects.FirstOrDefault(project => project.Id == _selectedWorkProjectId));
    private void SelectProjectChat(ProjectState project, ProjectChatState chat) =>
        QueueFixture(_projects, chat, selectedProjectId: project.IsFolderlessWorkspace ? null : project.Id);
    private void SelectWorkspaceProject(ProjectState project) => QueueFixture(_projects, null, selectedProjectId: project.Id);
    private static Task RunProjectActionAsync(Func<Task> action) => action();
    private Task SaveProjectsAndRenderAsync() { RenderProjects(); return Task.CompletedTask; }
    public bool WasCollapsedByUser(Guid id) => _collapsedByUser.Contains(id);

    public void QueueFixture(List<ProjectState> projects, ProjectChatState? active, Guid? expand = null,
        Guid? selectedProjectId = null)
    {
        _projects = projects;
        _activeProjectChat = active;
        _selectedWorkProjectId = selectedProjectId ?? projects.FirstOrDefault(project =>
            !project.IsFolderlessWorkspace && project.Chats.Any(chat => chat.Id == active?.Id))?.Id;
        RenderProjects(expand);
    }

    public void SetReplying(Guid chatId, bool replying)
    {
        if (replying) _pendingReplies[chatId] = new object();
        else _pendingReplies.Remove(chatId);
        RenderProjects();
    }

    public bool CheckUnloadedFocus()
    {
        var unloaded = new Grid { Tag = new ProjectTreeEntry(new ProjectState { Name = "Unloaded" }) };
        ProjectRow_LostFocus(unloaded, new RoutedEventArgs());
        ProjectRow_PointerExited(unloaded, null!);
        UpdateProjectRowActions(unloaded);
        return !ContainsKeyboardFocus(unloaded);
    }

    public void CloseView() => _projectViewClosed = true;
}
