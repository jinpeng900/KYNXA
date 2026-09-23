using System.Collections.ObjectModel;
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
    private readonly ProjectStore _projectStore = new(ApplicationData.Current.LocalFolder.Path);
    private List<ProjectState> _projects = [];
    public ObservableCollection<ProjectTreeEntry> ProjectEntries { get; } = [];

    private ProjectChatState? _activeProjectChat;
    private Action? _undoSidebarChange;
    private bool _projectsReady;
    private bool _projectActionPending;

    private async Task InitializeProjectsAsync()
    {
        if (_projectsReady) return;
        try
        {
            if (_projectStore.Exists) _projects = _projectStore.Load();
            else
            {
                _projects = ViewModel.Projects.Select(sample => new ProjectState
                {
                    Name = sample.Name,
                    Chats = sample.Conversations.Select(title => new ProjectChatState { Title = title, IsSample = true }).ToList()
                }).ToList();
                _projectStore.Save(_projects);
            }
            // Migrate the previously shipped demo rows; discard legacy empty new chats.
            foreach (var project in _projects)
            {
                var sample = ViewModel.Projects.FirstOrDefault(p => p.Name == project.Name);
                foreach (var chat in project.Chats)
                    if (sample?.Conversations.Contains(chat.Title) == true) chat.IsSample = true;
                project.Chats.RemoveAll(chat => !chat.CanPersist);
            }
            InitializeStandaloneChats();
            _projectsReady = true;
            RenderProjects();
            App.Window.Closed += (_, _) =>
            {
                StopMockReplies();
                CaptureProjectDraft();
                CaptureStandaloneDraft();
                try { _projectStore.Save(_projects); _projectStore.SaveChats(_standaloneChats); }
                catch (IOException) { /* Keep the previous complete catalog if the disk becomes unavailable. */ }
                catch (UnauthorizedAccessException) { }
            };
        }
        catch (Exception error)
        {
            AddProjectButton.IsEnabled = false;
            await ShowProjectErrorAsync("无法读取项目", error.Message);
        }
    }

    private void RenderProjects(Guid? expandProject = null)
    {
        var expanded = ProjectEntries.Where(p => p.IsExpanded).Select(p => p.Project.Id).ToHashSet();
        if (expandProject is Guid id) expanded.Add(id);
        ProjectEntries.Clear();
        _hoveredProjectRows.Clear();
        foreach (ProjectState project in _projects.Where(p => !p.IsArchived && !p.IsFolderlessWorkspace).OrderByDescending(p => p.IsPinned))
        {
            var entry = new ProjectTreeEntry(project) { IsExpanded = expanded.Contains(project.Id) };
            foreach (ProjectChatState chat in project.Chats.Where(c => !c.IsArchived).OrderByDescending(c => c.IsPinned))
                entry.Children.Add(new ProjectTreeEntry(project, chat));
            ProjectEntries.Add(entry);
        }
        RebuildWorkTasks();
        DispatcherQueue.TryEnqueue(() =>
        {
            if (_activeProjectChat is not null)
                ProjectTree.SelectedItem = ProjectEntries.SelectMany(p => p.Children).FirstOrDefault(p => p.Chat?.Id == _activeProjectChat.Id);
        });
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
            _projectMenuRow = null;
            if (row is not null) UpdateProjectRowActions(row);
        };
        menu.ShowAt(button);
    }

    private async void ProjectAddChat_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: ProjectTreeEntry entry }) return;
        await RunProjectActionAsync(() =>
        {
            CaptureProjectDraft();
            DiscardEmptyProjectChats();
            var project = entry.Project;
            string title = "新聊天";
            for (int number = 2; project.Chats.Any(c => c.Title == title); number++) title = $"新聊天 {number}";
            var chat = new ProjectChatState { Title = title };
            project.Chats.Insert(0, chat);
            ShowProjects();
            RenderProjects(project.Id);
            SelectProjectChat(project, chat);
            return Task.CompletedTask;
        });
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
        Item(project.IsPinned ? "取消置顶" : "置顶项目", "\uE718", () =>
        {
            project.IsPinned = !project.IsPinned;
            SaveProjectsAndRender();
            return Task.CompletedTask;
        });
        Item("在文件资源管理器中打开", "\uE8B7", async () =>
        {
            if (string.IsNullOrEmpty(project.FolderPath))
            {
                project.FolderPath = _projectStore.CreateManagedFolder(project.Id);
                _projectStore.Save(_projects);
            }
            if (!Directory.Exists(project.FolderPath)) throw new DirectoryNotFoundException($"关联文件夹不存在：{project.FolderPath}");
            var folder = await StorageFolder.GetFolderFromPathAsync(project.FolderPath);
            if (!await Windows.System.Launcher.LaunchFolderAsync(folder)) throw new IOException("无法打开文件资源管理器。");
        });
        Item("重命名项目", "\uE70F", async () =>
        {
            string? name = await AskProjectNameAsync("重命名项目", project.Name, "保存");
            if (name is null) return;
            project.Name = name;
            if (project.Chats.Contains(_activeProjectChat!))
            {
                _workConversationTitle = $"{project.Name} / {_activeProjectChat!.Title}";
                UpdateConversationTitle();
            }
            SaveProjectsAndRender();
        });
        Item("归档", "\uE7B8", () =>
        {
            CaptureProjectDraft();
            project.IsArchived = true;
            _projectStore.Save(_projects);
            _undoSidebarChange = () =>
            {
                project.IsArchived = false;
                SaveProjectsAndRender();
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
            RenderProjects();
            ProjectNotice.Message = $"已归档“{project.Name}”";
            ProjectNotice.IsOpen = true;
            return Task.CompletedTask;
        });
        return menu;
    }

    private async void NewBlankProject_Click(object sender, RoutedEventArgs e) => await RunProjectActionAsync(async () =>
    {
        if (await CreateBlankProjectAsync() is not { } project) return;
        ShowProjects();
        RevealProject(project);
    });

    private async Task<ProjectState?> CreateBlankProjectAsync()
    {
        string? name = await AskProjectNameAsync("新建空白项目", "", "创建");
        if (name is null) return null;
        var project = new ProjectState { Name = name };
        project.FolderPath = _projectStore.CreateManagedFolder(project.Id);
        _projects.Insert(0, project);
        SaveProjectsAndRender();
        return project;
    }

    private async void UseExistingProjectFolder_Click(object sender, RoutedEventArgs e) => await RunProjectActionAsync(async () =>
    {
        if (await PickProjectFolderAsync() is not { } project) return;
        ShowProjects();
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
        SaveProjectsAndRender();
        return project;
    }

    private async void UndoSidebarChange_Click(object sender, RoutedEventArgs e) => await RunProjectActionAsync(() =>
    {
        _undoSidebarChange?.Invoke();
        ProjectNotice.IsOpen = false;
        _undoSidebarChange = null;
        return Task.CompletedTask;
    });

    private void SaveProjectsAndRender()
    {
        CaptureProjectDraft();
        _projectStore.Save(_projects);
        RenderProjects();
    }

    private void ShowProjects()
    {
        ProjectTree.Visibility = Visibility.Visible;
        ProjectsChevron.Glyph = "\uE70D";
    }

    private void RevealProject(ProjectState project) => DispatcherQueue.TryEnqueue(() =>
    {
        var entry = ProjectEntries.FirstOrDefault(p => p.Project.Id == project.Id);
        if (entry is not null)
        {
            ProjectTree.SelectedItem = entry;
            if (ProjectTree.ContainerFromItem(entry) is FrameworkElement row) row.StartBringIntoView();
        }
    });
    private void CaptureProjectDraft()
    {
        if (!ViewModel.IsChatMode && _activeProjectChat is not null) _activeProjectChat.Draft = PromptTextBox.Text;
    }

    private void SelectProjectChat(ProjectState project, ProjectChatState chat)
    {
        CaptureProjectDraft();
        DiscardEmptyProjectChats(chat.Id);
        _activeProjectChat = chat;
        _workWithoutFolder = project.IsFolderlessWorkspace;
        _workConversationTitle = WorkChatTitle(project, chat);
        _workDraft = chat.Draft;
        SetPrimaryMode(false);
        PromptTextBox.Text = ViewModel.Prompt = chat.Draft;
        UpdateConversationTitle();
        UpdateConversationPresentation();
        ProjectTree.SelectedItem = ProjectEntries.SelectMany(p => p.Children).FirstOrDefault(p => p.Chat?.Id == chat.Id);
        WorkTaskHistory.SelectedItem = WorkTaskEntries.FirstOrDefault(task => task.Chat?.Id == chat.Id);
        PromptTextBox.Focus(FocusState.Programmatic);
    }

    private void ProjectTree_ItemInvoked(TreeView sender, TreeViewItemInvokedEventArgs args)
    {
        if (args.InvokedItem is not ProjectTreeEntry entry) return;
        if (entry.Chat is null) entry.IsExpanded = !entry.IsExpanded;
        else SelectProjectChat(entry.Project, entry.Chat);
    }
    private async Task<string?> AskProjectNameAsync(string title, string value, string primary)
    {
        var input = new TextBox { FontFamily = (FontFamily)Application.Current.Resources["KynxaUIFont"], Text = value, PlaceholderText = "输入项目名称", MaxLength = 80, MinWidth = 300 };
        AutomationProperties.SetAutomationId(input, "ProjectNameInput");
        var dialog = new ContentDialog { XamlRoot = XamlRoot, Title = title, Content = input,
            PrimaryButtonText = primary, CloseButtonText = "取消", DefaultButton = ContentDialogButton.Primary,
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
                _projects = _projectStore.Load();
                _standaloneChats = _projectStore.LoadChats();
                _activeProjectChat = _activeStandaloneChat = null;
                RenderProjects();
                RebuildStandaloneRows();
                UpdateConversationPresentation();
            }
            catch (Exception) { /* Preserve the current display if the catalog itself is unavailable. */ }
            await ShowProjectErrorAsync("项目操作未完成", error.Message);
        }
        finally { _projectActionPending = false; }
    }

    private async Task ShowProjectErrorAsync(string title, string message) => await new ContentDialog
    {
        XamlRoot = XamlRoot, Title = title, Content = message, CloseButtonText = "知道了"
    }.ShowAsync();
}
