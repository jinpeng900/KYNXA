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

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private ProjectStore _projectStore = new(StoragePaths.DesktopDirectory);
    private List<ProjectState> _projects = [];
    public ObservableCollection<ProjectTreeEntry> ProjectEntries { get; } = [];

    private ProjectChatState? _activeProjectChat;
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
            App.Window.AppWindow.Closing += MainWindow_Closing;
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
            // Loading metadata cannot discard a chat draft created while startup was waiting on the gateway.
            // 启动等待网关时新建的聊天草稿仍属于用户，目录返回不能丢掉它及当前对象身份。
            var drafts = _standaloneChats.Where(chat => !chat.IsPersisted
                && !catalog.Chats.Any(saved => saved.Id == chat.Id)).ToArray();
            _projects = catalog.Projects;
            _standaloneChats = drafts.Concat(catalog.Chats).ToList();
            RebuildStandaloneRows();
            _projectsReady = true;
            RenderProjects();
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
        CloseSidebarSearch("tasks");
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
        bool hasFolder = !string.IsNullOrWhiteSpace(ProjectMountPresentation.WorkspaceMount(project)?.Path ?? project.FolderPath);
        Item(hasFolder ? UiText.Get("重新关联文件夹") : UiText.Get("关联工作文件夹"), "\uE8F4", () => ChangeProjectFolderAsync(project));
        if (hasFolder)
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
            Guid projectId = project.Id;
            Func<Task> undo = async () =>
            {
                var current = _projects.FirstOrDefault(candidate => candidate.Id == projectId && candidate.IsArchived)
                    ?? throw ProjectUndoUnavailable();
                current.IsArchived = false;
                try { await SaveProjectsAndRenderAsync(); }
                catch { current.IsArchived = true; RenderProjects(); throw; }
                RevealProject(current);
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
            ShowProjectNotice(string.Format(UiText.Get("已归档“{0}”"), project.Name), undo);
        });
        return menu;
    }

    private bool IsAvailableProject(ProjectState project) => !_projectViewClosed && !project.IsArchived && !project.IsFolderlessWorkspace && _projects.Contains(project);

    private async Task OpenProjectFolderAsync(ProjectState project)
    {
        if (!IsAvailableProject(project)) return;
        string? path = ProjectMountPresentation.WorkspaceMount(project)?.Path ?? project.FolderPath;
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

    private Task ChangeProjectFolderAsync(ProjectState project)
    {
        if (!IsAvailableProject(project)) return Task.CompletedTask;
        Guid? originalChat = ActiveChatId, originalProject = _selectedWorkProjectId;
        bool originalMode = ViewModel.IsChatMode;
        var editor = new WorkspaceFolderEditor(UiText.Get("关联工作文件夹"), project.Name,
            ProjectMountPresentation.WorkspaceMount(project)?.Path ?? project.FolderPath, creating: false, async input =>
            {
                if (!IsAvailableProject(project) || ActiveChatId != originalChat || _selectedWorkProjectId != originalProject ||
                    ViewModel.IsChatMode != originalMode) throw new OperationCanceledException();
                await CommitFolderEditAsync(() => input.FolderPath is { } path
                    ? ReplaceProjectFolderAsync(project, path) : UnmountProjectFolderAsync(project, showFailureEditor: false));
            }, allowUnlink: !string.IsNullOrWhiteSpace(project.FolderPath));
        ShowInlineFolderEditor(editor);
        return Task.CompletedTask;
    }

    private async Task ReplaceProjectFolderAsync(ProjectState project, string path)
    {
        var previous = CaptureProjectFolderBinding(project);
        var original = ProjectMountPresentation.WorkspaceMount(project);
        bool unchanged = original is not null && string.Equals(Path.TrimEndingDirectorySeparator(original.Path),
            path, StringComparison.OrdinalIgnoreCase);
        var replacement = original is null ? new ProjectMountState(Guid.NewGuid(), FolderDisplayName(path), path)
            : original with { Name = FolderDisplayName(path), Path = path, BindingRevision = original.BindingRevision + (unchanged ? 0UL : 1UL) };
        project.Mounts = [replacement];
        project.DefaultMountId = replacement.Id;
        project.FolderPath = replacement.Path;
        try { await SaveProjectsAndRenderAsync(); }
        catch { ApplyProjectFolderBinding(project, previous); throw; }
        ShowProjectFolderNotice(project, previous,
            string.Format(UiText.Get("“{0}”已关联到 {1}，聊天和原文件夹内容保持不变。"), project.Name, path));
    }

    private Task UnmountProjectFolderAsync(ProjectState project) => UnmountProjectFolderAsync(project, showFailureEditor: true);

    private async Task UnmountProjectFolderAsync(ProjectState project, bool showFailureEditor)
    {
        if (!IsAvailableProject(project) || string.IsNullOrWhiteSpace(ProjectMountPresentation.WorkspaceMount(project)?.Path ?? project.FolderPath)) return;
        var previous = CaptureProjectFolderBinding(project);
        project.Mounts = [];
        project.DefaultMountId = null;
        project.FolderPath = null;
        try { await SaveProjectsAndRenderAsync(); }
        catch (Exception error)
        {
            ApplyProjectFolderBinding(project, previous);
            if (!showFailureEditor) throw;
            // Keep this chat and draft in place; retry errors belong to the same inline folder editor.
            await ChangeProjectFolderAsync(project);
            WorkspaceHome.ShowFolderEditorError(error.Message);
            return;
        }
        ShowProjectFolderNotice(project, previous,
            string.Format(UiText.Get("“{0}”已取消文件夹关联，聊天和文件保持不变。"), project.Name));
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

    private Task<ProjectState?> PickProjectFolderAsync() => ShowProjectCreationAsync(reuseFolderProject: true);

    private async Task SaveProjectsAndRenderAsync()
    {
        CaptureProjectDraft();
        await _projectStore.SaveAsync(_projects);
        RenderProjects();
        UpdateMountedWorkspacePresentation();
    }

    private void ShowProjects()
    {
        CloseSidebarSearch("projects");
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
        UpdateProjectNoticeAction();
        UpdateSendButtonState();
        try { await action(); }
        catch (Exception error)
        {
            if (_savingOnClose || _projectViewClosed) return;
            if (!_projectNoticeUndoPending)
            {
                _undoSidebarChange = null;
                ProjectNotice.IsOpen = false;
            }
            // Roll back visible metadata to the last successfully saved catalog.
            // 将可见元数据回退到最近一次成功保存的正式目录。
            try
            {
                var catalog = await _projectStore.LoadMetadataAsync();
                if (_savingOnClose || _projectViewClosed) return;
                PreservePendingPresentations(catalog);
                if (_projectNoticeUndoPending) PreserveUndoConversationProjections(catalog);
                else _activeProjectChat = _activeStandaloneChat = null;
                _projects = catalog.Projects;
                _standaloneChats = catalog.Chats;
                RenderProjects();
                RebuildStandaloneRows();
                if (_projectNoticeUndoPending) UpdateConversationTitle();
                UpdateConversationPresentation();
            }
            catch (Exception) { /* Preserve the current display if the catalog itself is unavailable. 中文：正式目录本身不可用时，保留当前展示。 */ }
            await ShowProjectErrorAsync(UiText.Get("项目操作未完成"), error.Message);
        }
        finally
        {
            _projectActionPending = false;
            UpdateProjectNoticeAction();
            if (!_savingOnClose && !_projectViewClosed) UpdateSendButtonState();
        }
    }

    private async Task ShowProjectErrorAsync(string title, string message)
    {
        if (_savingOnClose || _projectViewClosed)
        {
            System.Diagnostics.Debug.WriteLine($"Project operation during exit: {title}");
            return;
        }
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
