using System.Collections.ObjectModel;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.ViewModels;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using Windows.Storage;
using Windows.Storage.Pickers;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private ProjectStore _projectStore = new(StoragePaths.DesktopDirectory);
    private List<ProjectState> _projects = [];
    public ObservableCollection<ProjectTreeEntry> ProjectEntries { get; } = [];

    private ProjectChatState? _activeProjectChat;
    private Func<Task>? _undoSidebarChange;
    private bool _projectsReady;
    private bool _projectActionPending;
    private bool _projectViewClosed;
    private bool _projectLifetimeAttached;

    private async Task InitializeProjectsAsync()
    {
        if (_projectsReady) return;
        if (!_projectLifetimeAttached)
        {
            _projectLifetimeAttached = true;
            App.Window.Closed += (_, _) =>
            {
                _projectViewClosed = true;
                _pendingProjectExpansions.Clear();
                _projectToReveal = null;
                ClearProjectOrdering();
                _hoveredProjectRows.Clear();
                _orderRows.Clear();
                _projectMenuRow = null;
                StopReplies();
                StopHistoryLoads();
                _projectStore.Dispose();
            };
        }
        try
        {
            var catalog = await _projectStore.LoadMetadataAsync();
            if (_projectViewClosed) return;
            _projects = catalog.Projects;
            _standaloneChats = catalog.Chats;
            RebuildStandaloneRows();
            _projectsReady = true;
            RenderProjects();
            App.Window.AppWindow.Closing += SaveBeforeClosing;
        }
        catch (Exception error)
        {
            if (_projectViewClosed) return;
            AddProjectButton.IsEnabled = false;
            await ShowProjectErrorAsync(UiText.Get("无法读取项目"), error.Message);
        }
    }

    private void ProjectMore_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: ProjectTreeEntry entry } button) return;
        ShowSidebarMenu(button, entry.Chat is null ? CreateProjectMenu(entry.Project) : CreateChatMenu(entry.Chat, entry.Project));
    }

    private void ShowSidebarMenu(Button button, MenuFlyout menu)
    {
        var row = FindProjectRow(button);
        _projectMenuRow = row;
        if (row is not null) UpdateProjectRowActions(row);
        menu.Closed += (_, _) =>
        {
            if (_projectMenuRow == row) _projectMenuRow = null;
            if (row is not null) UpdateProjectRowActions(row);
        };
        menu.ShowAt(button);
    }

    private async void ProjectAddChat_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: ProjectTreeEntry entry }) return;
        await RunProjectActionAsync(() =>
        {
            StartNewProjectChat(entry.Project);
            return Task.CompletedTask;
        });
    }

    // Both sidebar add actions create the same RAM-only draft; the first user message commits it.
    // 两处侧栏新增动作创建同一类仅在内存中的草稿；首条用户消息才提交保存。
    private void StartNewProjectChat(ProjectState project)
    {
        if (project.IsArchived || project.IsFolderlessWorkspace || !_projects.Contains(project)) return;
        CaptureProjectDraft();
        DiscardEmptyProjectChats();
        string title = UiText.Get("新聊天");
        for (int number = 2; project.Chats.Any(chat => chat.Title == title); number++) title = string.Format(UiText.Get("新聊天 {0}"), number);
        var chat = new ProjectChatState { Title = title };
        project.Chats.Insert(0, chat);
        SelectProjectChat(project, chat);
    }
    private MenuFlyout CreateProjectMenu(ProjectState project)
    {
        var menu = new MenuFlyout
        {
            Placement = FlyoutPlacementMode.BottomEdgeAlignedLeft,
            MenuFlyoutPresenterStyle = (Style)Application.Current.Resources["KynxaProjectMenuPresenterStyle"]
        };
        void Item(string title, string glyph, Func<Task> action)
        {
            var item = new MenuFlyoutItem { Text = title, Icon = new FontIcon { Glyph = glyph },
                Style = (Style)Application.Current.Resources["KynxaWorkMenuItemStyle"] };
            item.Click += async (_, _) => await RunProjectActionAsync(action);
            menu.Items.Add(item);
        }
        Item(project.IsPinned ? UiText.Get("取消置顶") : UiText.Get("置顶项目"), "\uE718", async () =>
        {
            project.IsPinned = !project.IsPinned;
            await SaveProjectsAndRenderAsync();
        });
        Item(UiText.Get("在文件资源管理器中打开"), "\uE8B7", () => OpenProjectFolderAsync(project));
        Item(string.IsNullOrWhiteSpace(project.FolderPath) ? UiText.Get("关联工作文件夹") : UiText.Get("重新关联文件夹"), "\uE8F4", () => ChangeProjectFolderAsync(project));
        if (!string.IsNullOrWhiteSpace(project.FolderPath))
            Item(UiText.Get("取消关联文件夹"), "\uE8F4", () => UnmountProjectFolderAsync(project));
        Item(UiText.Get("检索设置"), "\uE721", () =>
        {
            OpenRetrievalSettings(project);
            return Task.CompletedTask;
        });
        Item(UiText.Get("重命名项目"), "\uE70F", async () =>
        {
            string? name = await AskProjectNameAsync(UiText.Get("重命名项目"), project.Name, UiText.Get("保存"));
            if (name is null) return;
            project.Name = name;
            if (project.Chats.Contains(_activeProjectChat!))
            {
                _workConversationTitle = $"{project.Name} / {_activeProjectChat!.Title}";
                UpdateConversationTitle();
            }
            else if (_selectedWorkProjectId == project.Id)
            {
                _workConversationTitle = project.Name;
                UpdateConversationTitle();
            }
            await SaveProjectsAndRenderAsync();
        });
        Item(UiText.Get("归档"), "\uE7B8", async () =>
        {
            CaptureProjectDraft();
            project.IsArchived = true;
            await _projectStore.SaveAsync(_projects);
            _undoSidebarChange = async () =>
            {
                project.IsArchived = false;
                await SaveProjectsAndRenderAsync();
                RevealProject(project);
            };
            if (project.Chats.Contains(_activeProjectChat!))
            {
                _activeProjectChat = null;
                _workConversationTitle = _workDraft = string.Empty;
                PromptTextBox.Text = ViewModel.Prompt = string.Empty;
                UpdateConversationTitle();
                UpdateConversationPresentation();
            }
            if (_selectedWorkProjectId == project.Id)
            {
                _selectedWorkProjectId = null;
                _workConversationTitle = _workDraft = string.Empty;
                if (!ViewModel.IsChatMode) PromptTextBox.Text = ViewModel.Prompt = string.Empty;
                UpdateConversationTitle();
                UpdateWorkspacePickerVisibility();
            }
            RenderProjects();
            ProjectNotice.Message = string.Format(UiText.Get("已归档“{0}”"), project.Name);
            ProjectNotice.IsOpen = true;
        });
        return menu;
    }

    private bool IsAvailableProject(ProjectState project) => !_projectViewClosed && !project.IsArchived && !project.IsFolderlessWorkspace && _projects.Contains(project);

    private async Task OpenProjectFolderAsync(ProjectState project)
    {
        if (!IsAvailableProject(project)) return;
        string? path = project.Mounts.FirstOrDefault(mount => mount.Id == project.DefaultMountId)?.Path ?? project.FolderPath;
        if (string.IsNullOrWhiteSpace(path))
        {
            // A blank project keeps its managed runtime directory without turning it into an external mount.
            // 空白项目保留运行时托管目录，不为打开资源管理器而新建并写入虚假的外部挂载。
            var chat = project.Chats.FirstOrDefault(chat => chat.Id == ActiveChatId && chat.IsPersisted) ??
                project.Chats.FirstOrDefault(chat => chat.IsPersisted);
            if (chat is null) { ShowActionFeedback(UiText.Get("先在项目中发送消息，再打开工作文件夹。")); return; }
            using var api = new WorkspaceFilesApiClient();
            path = (await api.ListAsync(chat.Id, null, null, _historyLifetime.Token)).Path;
        }
        if (!Directory.Exists(path)) throw new DirectoryNotFoundException(string.Format(UiText.Get("关联文件夹不存在：{0}"), path));
        var folder = await StorageFolder.GetFolderFromPathAsync(path);
        if (!await Windows.System.Launcher.LaunchFolderAsync(folder)) throw new IOException(UiText.Get("无法打开文件资源管理器。"));
    }

    private async Task ChangeProjectFolderAsync(ProjectState project)
    {
        if (!IsAvailableProject(project)) return;
        var picker = new FolderPicker { SuggestedStartLocation = PickerLocationId.DocumentsLibrary };
        picker.FileTypeFilter.Add("*");
        WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(App.Window));
        var folder = await picker.PickSingleFolderAsync();
        if (folder is null || !IsAvailableProject(project)) return;
        string? previous = project.FolderPath;
        var previousMounts = project.Mounts.ToList();
        Guid? previousDefault = project.DefaultMountId;
        var original = project.Mounts.FirstOrDefault(mount => mount.Id == project.DefaultMountId);
        var replacement = original is null ? new ProjectMountState(Guid.NewGuid(), folder.DisplayName, folder.Path)
            : original with { Name = folder.DisplayName, Path = folder.Path, BindingRevision = original.BindingRevision + 1 };
        project.Mounts.RemoveAll(mount => mount.Id == replacement.Id);
        project.Mounts.Add(replacement);
        project.DefaultMountId = replacement.Id;
        project.FolderPath = replacement.Path;
        try { await SaveProjectsAndRenderAsync(); }
        catch { project.FolderPath = previous; project.Mounts = previousMounts; project.DefaultMountId = previousDefault; throw; }
        ProjectNotice.Message = string.Format(UiText.Get("“{0}”已关联到 {1}，聊天和原文件夹内容保持不变。"), project.Name, folder.Path);
        ProjectNotice.IsOpen = true;
    }

    private async Task UnmountProjectFolderAsync(ProjectState project)
    {
        if (!IsAvailableProject(project) || string.IsNullOrWhiteSpace(project.FolderPath)) return;
        string? previous = project.FolderPath;
        var previousMounts = project.Mounts.ToList();
        Guid? previousDefault = project.DefaultMountId;
        project.Mounts.RemoveAll(mount => mount.Id == project.DefaultMountId);
        project.DefaultMountId = project.Mounts.FirstOrDefault()?.Id;
        project.FolderPath = project.Mounts.FirstOrDefault()?.Path;
        try { await SaveProjectsAndRenderAsync(); }
        catch { project.FolderPath = previous; project.Mounts = previousMounts; project.DefaultMountId = previousDefault; throw; }
        ProjectNotice.Message = string.Format(UiText.Get("“{0}”已取消文件夹关联，聊天和文件保持不变。"), project.Name);
        ProjectNotice.IsOpen = true;
    }

    private async Task RunMountedWorkspaceActionAsync(MountedWorkspaceRequest request, Func<ProjectState, Task> action)
    {
        await RunProjectActionAsync(() =>
        {
            var project = WorkSidebarState.FindSelectedProject(_projects, _selectedWorkProjectId);
            if (ViewModel.IsChatMode || project is null || project.Id != request.ProjectId || !IsAvailableProject(project) ||
                !string.Equals(UserMountedFolder(project), request.FolderPath, StringComparison.OrdinalIgnoreCase)) return Task.CompletedTask;
            return action(project);
        });
    }

    private async void MountedWorkspaceOpenRequested(object? sender, MountedWorkspaceRequest request) =>
        await RunMountedWorkspaceActionAsync(request, OpenProjectFolderAsync);
    private async void MountedWorkspaceChangeRequested(object? sender, MountedWorkspaceRequest request) =>
        await RunMountedWorkspaceActionAsync(request, ChangeProjectFolderAsync);
    private async void MountedWorkspaceUnmountRequested(object? sender, MountedWorkspaceRequest request) =>
        await RunMountedWorkspaceActionAsync(request, UnmountProjectFolderAsync);

    private async void NewBlankProject_Click(object sender, RoutedEventArgs e)
    {
        if (await CreateBlankProjectAsync() is not { } project) return;
        ShowProjects();
        SelectWorkspaceProject(project);
        RevealProject(project);
    }

    private Task<ProjectState?> CreateBlankProjectAsync() => ShowProjectCreationAsync();

    private async void UseExistingProjectFolder_Click(object sender, RoutedEventArgs e)
    {
        if (await PickProjectFolderAsync() is not { } project) return;
        ShowProjects();
        SelectWorkspaceProject(project);
        RevealProject(project);
    }

    private async Task<ProjectState?> PickProjectFolderAsync()
    {
        Guid? originalChat = ActiveChatId, originalProject = _selectedWorkProjectId;
        bool originalMode = ViewModel.IsChatMode;
        var picker = new FolderPicker { SuggestedStartLocation = PickerLocationId.DocumentsLibrary };
        picker.FileTypeFilter.Add("*");
        WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(App.Window));
        StorageFolder? folder = await picker.PickSingleFolderAsync();
        if (folder is null || _projectViewClosed || _chatClosing || _savingOnClose || ActiveChatId != originalChat ||
            _selectedWorkProjectId != originalProject || ViewModel.IsChatMode != originalMode) return null;
        var project = _projects.FirstOrDefault(candidate => candidate.Mounts.Any(mount =>
            string.Equals(Path.TrimEndingDirectorySeparator(mount.Path), Path.TrimEndingDirectorySeparator(folder.Path), StringComparison.OrdinalIgnoreCase)) ||
            candidate.FolderPath is not null && string.Equals(Path.TrimEndingDirectorySeparator(candidate.FolderPath),
                Path.TrimEndingDirectorySeparator(folder.Path), StringComparison.OrdinalIgnoreCase));
        if (project is null) return await ShowProjectCreationAsync(folder);
        await RunProjectActionAsync(async () => { project.IsArchived = false; await SaveProjectsAndRenderAsync(); });
        return project;
    }

    private async void UndoSidebarChange_Click(object sender, RoutedEventArgs e) => await RunProjectActionAsync(async () =>
    {
        if (_undoSidebarChange is { } undo) await undo();
        ProjectNotice.IsOpen = false;
        _undoSidebarChange = null;
    });

    private async Task SaveProjectsAndRenderAsync()
    {
        CaptureProjectDraft();
        await _projectStore.SaveAsync(_projects);
        RenderProjects();
        UpdateMountedWorkspacePresentation();
    }

    private void ShowProjects()
    {
        ProjectTree.Visibility = Visibility.Visible;
        ProjectsChevron.Glyph = "\uE70D";
        UpdateWorkSidebarHeights(WorkSidebarContent.ActualHeight);
    }

    private void RevealProject(ProjectState project)
    {
        _projectToReveal = project.Id;
        RenderProjects();
    }
    private void CaptureProjectDraft()
    {
        if (!ViewModel.IsChatMode && _activeProjectChat is not null) _activeProjectChat.Draft = PromptTextBox.Text;
    }

    private void SelectProjectChat(ProjectState project, ProjectChatState chat)
    {
        CaptureProjectDraft();
        DiscardEmptyProjectChats(chat.Id);
        _activeProjectChat = chat;
        PositionConversationComposer(chat);
        _selectedWorkProjectId = project.IsFolderlessWorkspace ? null : project.Id;
        _workChatToReveal = chat.Id;
        _projectToReveal = project.IsFolderlessWorkspace ? null : project.Id;
        RenderProjects();
        _workWithoutFolder = project.IsFolderlessWorkspace;
        _workConversationTitle = WorkChatTitle(project, chat);
        _workDraft = chat.Draft;
        SetPrimaryMode(false, updateConversation: false);
        PromptTextBox.Text = ViewModel.Prompt = chat.Draft;
        UpdateConversationTitle();
        UpdateConversationPresentation();
        PromptTextBox.Focus(FocusState.Programmatic);
    }

    private void ProjectTree_ItemInvoked(TreeView sender, TreeViewItemInvokedEventArgs args)
    {
        if (args.InvokedItem is not ProjectTreeEntry entry) return;
        if (entry.Chat is null) SelectWorkspaceProject(entry.Project);
        else SelectProjectChat(entry.Project, entry.Chat);
    }
    private async Task<string?> AskProjectNameAsync(string title, string value, string primary)
    {
        var input = new TextBox { FontFamily = (FontFamily)Application.Current.Resources["KynxaUIFont"], Text = value, PlaceholderText = UiText.Get("输入项目名称"), MaxLength = 80, MinWidth = 300 };
        AutomationProperties.SetAutomationId(input, "ProjectNameInput");
        var dialog = new ContentDialog { XamlRoot = XamlRoot, Title = title, Content = input,
            PrimaryButtonText = primary, CloseButtonText = UiText.Get("取消"), DefaultButton = ContentDialogButton.Primary,
            IsPrimaryButtonEnabled = !string.IsNullOrWhiteSpace(value) };
        input.TextChanged += (_, _) => dialog.IsPrimaryButtonEnabled = !string.IsNullOrWhiteSpace(input.Text);
        dialog.Opened += (_, _) => { input.Focus(FocusState.Programmatic); input.SelectAll(); };
        return await DialogPresentation.ShowAsync(dialog) == ContentDialogResult.Primary ? input.Text.Trim() : null;
    }

    private async Task RunProjectActionAsync(Func<Task> action)
    {
        if (!_projectsReady || _projectActionPending || _savingOnClose || _chatClosing) return;
        _projectActionPending = true;
        UpdateSendButtonState();
        try { await action(); }
        catch (Exception error)
        {
            _undoSidebarChange = null;
            ProjectNotice.IsOpen = false;
            // Roll back visible metadata to the last successfully saved catalog.
            // 将可见元数据回退到最近一次成功保存的正式目录。
            try
            {
                var catalog = await _projectStore.LoadMetadataAsync();
                PreservePendingPresentations(catalog);
                _projects = catalog.Projects;
                _standaloneChats = catalog.Chats;
                _activeProjectChat = _activeStandaloneChat = null;
                RenderProjects();
                RebuildStandaloneRows();
                UpdateConversationPresentation();
            }
            catch (Exception) { /* Preserve the current display if the catalog itself is unavailable. 中文：正式目录本身不可用时，保留当前展示。 */ }
            await ShowProjectErrorAsync(UiText.Get("项目操作未完成"), error.Message);
        }
        finally
        {
            _projectActionPending = false;
            UpdateSendButtonState();
        }
    }

    private async Task ShowProjectErrorAsync(string title, string message)
    {
        if (_savingOnClose) { _closeError = message; return; }
        await DialogPresentation.ShowAsync(new ContentDialog
        {
            XamlRoot = XamlRoot, Title = title, Content = message, CloseButtonText = UiText.Get("知道了")
        });
    }

    private void PreservePendingPresentations(ConversationCatalog catalog)
    {
        foreach (var chat in catalog.Chats.Concat(catalog.Projects.SelectMany(project => project.Chats)))
        {
            if (!_pendingReplies.TryGetValue(chat.Id, out var pending)) continue;
            int index = chat.Messages.FindIndex(message => message.Id == pending.Message.Id);
            if (index >= 0) chat.Messages[index] = pending.Message;
            else chat.Messages.Add(pending.Message);
        }
    }
}
