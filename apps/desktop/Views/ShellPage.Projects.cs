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
    private bool _savingOnClose;
    private bool _closeApproved;
    private bool _projectViewClosed;
    private bool _projectLifetimeAttached;

    private async void SaveBeforeClosing(Microsoft.UI.Windowing.AppWindow sender, Microsoft.UI.Windowing.AppWindowClosingEventArgs e)
    {
        if (_closeApproved) return;
        e.Cancel = true;
        if (_savingOnClose || StoragePaths.IsMigrating) return;
        if (_projectActionPending || _sendingPrompt)
        {
            ProjectNotice.Message = UiText.Get("正在保存聊天，请稍后再关闭。");
            ProjectNotice.IsOpen = true;
            return;
        }
        _savingOnClose = true;
        IsEnabled = false;
        try
        {
            CaptureProjectDraft();
            CaptureStandaloneDraft();
            await _projectStore.SaveAsync(_projects);
            await _projectStore.SaveChatsAsync(_standaloneChats);
            _closeApproved = true;
            App.Window.Close();
        }
        catch (Exception error)
        {
            var choice = await new ContentDialog
            {
                XamlRoot = XamlRoot, Title = UiText.Get("草稿保存未完成"), Content = error.Message + UiText.Get(" 已提交的聊天由会话服务保存，尚未保存的草稿或排序可能丢失。"),
                PrimaryButtonText = UiText.Get("仍然关闭"), CloseButtonText = UiText.Get("返回"), DefaultButton = ContentDialogButton.Close
            }.ShowAsync();
            if (choice == ContentDialogResult.Primary) { _closeApproved = true; App.Window.Close(); }
        }
        finally { _savingOnClose = false; if (!_closeApproved) IsEnabled = true; }
    }

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
                _projectStore.Dispose();
            };
        }
        try
        {
            var catalog = await _projectStore.LoadAsync();
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
        if (string.IsNullOrWhiteSpace(project.FolderPath))
        {
            string? previous = project.FolderPath;
            project.FolderPath = _projectStore.CreateManagedFolder(project.Id);
            try { await SaveProjectsAndRenderAsync(); }
            catch { project.FolderPath = previous; throw; }
        }
        string path = project.FolderPath ?? throw new DirectoryNotFoundException(UiText.Get("关联工作文件夹"));
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
        project.FolderPath = folder.Path;
        try { await SaveProjectsAndRenderAsync(); }
        catch { project.FolderPath = previous; throw; }
        ProjectNotice.Message = string.Format(UiText.Get("“{0}”已关联到 {1}，聊天和原文件夹内容保持不变。"), project.Name, folder.Path);
        ProjectNotice.IsOpen = true;
    }

    private async Task UnmountProjectFolderAsync(ProjectState project)
    {
        if (!IsAvailableProject(project) || string.IsNullOrWhiteSpace(project.FolderPath)) return;
        string? previous = project.FolderPath;
        project.FolderPath = null;
        try { await SaveProjectsAndRenderAsync(); }
        catch { project.FolderPath = previous; throw; }
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

    private async void NewBlankProject_Click(object sender, RoutedEventArgs e) => await RunProjectActionAsync(async () =>
    {
        if (await CreateBlankProjectAsync() is not { } project) return;
        ShowProjects();
        SelectWorkspaceProject(project);
        RevealProject(project);
    });

    private async Task<ProjectState?> CreateBlankProjectAsync()
    {
        string? name = await AskProjectNameAsync(UiText.Get("新建空白项目"), "", UiText.Get("创建"));
        if (name is null) return null;
        var project = new ProjectState { Name = name };
        project.FolderPath = _projectStore.CreateManagedFolder(project.Id);
        _projects.Insert(0, project);
        await SaveProjectsAndRenderAsync();
        return project;
    }

    private async void UseExistingProjectFolder_Click(object sender, RoutedEventArgs e) => await RunProjectActionAsync(async () =>
    {
        if (await PickProjectFolderAsync() is not { } project) return;
        ShowProjects();
        SelectWorkspaceProject(project);
        RevealProject(project);
    });

    private async Task<ProjectState?> PickProjectFolderAsync()
    {
        var picker = new FolderPicker { SuggestedStartLocation = PickerLocationId.DocumentsLibrary };
        picker.FileTypeFilter.Add("*");
        WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(App.Window));
        StorageFolder? folder = await picker.PickSingleFolderAsync();
        if (folder is null) return null;
        var project = _projects.FirstOrDefault(p => p.FolderPath is not null && string.Equals(
            Path.TrimEndingDirectorySeparator(p.FolderPath), Path.TrimEndingDirectorySeparator(folder.Path), StringComparison.OrdinalIgnoreCase));
        if (project is null)
        {
            project = new ProjectState { Name = folder.DisplayName, FolderPath = folder.Path };
            _projects.Insert(0, project);
        }
        project.IsArchived = false;
        await SaveProjectsAndRenderAsync();
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
        return await dialog.ShowAsync() == ContentDialogResult.Primary ? input.Text.Trim() : null;
    }

    private async Task RunProjectActionAsync(Func<Task> action)
    {
        if (!_projectsReady || _projectActionPending) return;
        _projectActionPending = true;
        try { await action(); }
        catch (Exception error)
        {
            _undoSidebarChange = null;
            ProjectNotice.IsOpen = false;
            // Roll back visible metadata to the last successfully saved catalog.
            try
            {
                var catalog = await _projectStore.LoadAsync();
                PreservePendingPresentations(catalog);
                _projects = catalog.Projects;
                _standaloneChats = catalog.Chats;
                _activeProjectChat = _activeStandaloneChat = null;
                RenderProjects();
                RebuildStandaloneRows();
                UpdateConversationPresentation();
            }
            catch (Exception) { /* Preserve the current display if the catalog itself is unavailable. */ }
            await ShowProjectErrorAsync(UiText.Get("项目操作未完成"), error.Message);
        }
        finally { _projectActionPending = false; }
    }

    private async Task ShowProjectErrorAsync(string title, string message) => await new ContentDialog
    {
        XamlRoot = XamlRoot, Title = title, Content = message, CloseButtonText = UiText.Get("知道了")
    }.ShowAsync();

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
