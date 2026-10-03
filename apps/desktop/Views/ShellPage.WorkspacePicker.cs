using System.Collections.ObjectModel;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.Layout;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private bool _workWithoutFolder;
    public ObservableCollection<ProjectTreeEntry> WorkTaskEntries { get; } = [];

    private static string WorkChatTitle(ProjectState project, ProjectChatState chat) =>
        project.IsFolderlessWorkspace ? chat.Title : $"{project.Name} / {chat.Title}";

    private void UpdateWorkspacePickerVisibility()
    {
        ComposerHost.IsFooterVisible = !ViewModel.IsChatMode && _activeProjectChat is null;
        WorkspacePickerButton.Visibility = ComposerHost.IsFooterVisible ? Visibility.Visible : Visibility.Collapsed;
        WorkspacePickerButton.IsEnabled = _projectsReady;
        WorkspacePickerLabel.Text = _workWithoutFolder ? "不使用文件夹" : "选择项目";
        AutomationProperties.SetName(WorkspacePickerButton, _workWithoutFolder ? "工作区：不使用文件夹" : "选择工作区");
    }

    private void WorkspacePickerButton_Click(object sender, RoutedEventArgs e) => ShowWorkspacePicker();

    private void ShowWorkspacePicker()
    {
        if (!_projectsReady || ViewModel.IsChatMode || _activeProjectChat is not null) return;
        var menu = PickerMenu.Create(FlyoutPlacementMode.BottomEdgeAlignedLeft, "KynxaWorkspaceFlyoutPresenterStyle");
        var choices = _projects.Where(project => !project.IsArchived && !project.IsFolderlessWorkspace)
            .OrderByDescending(project => project.IsPinned).ToList();
        var projects = PickerMenu.CreateList("WorkspaceProjectList", "已有项目", "KynxaWorkspaceListItemStyle", ListViewSelectionMode.None);
        double rowHeight = PickerMenu.Dimension("KynxaMenuRowHeight");
        projects.Height = Math.Min(Math.Max(rowHeight * 2, Math.Min(rowHeight * 6, XamlRoot.Size.Height - 200)), Math.Max(rowHeight, choices.Count * rowHeight));
        foreach (var project in choices)
        {
            var row = PickerMenu.LabelRow(project.Name, "\uE8B7");
            row.Tag = project;
            var item = new ListViewItem { Content = row, Tag = project };
            AutomationProperties.SetAutomationId(item, $"WorkspaceProject_{project.Id:N}");
            AutomationProperties.SetName(item, project.Name);
            ToolTipService.SetToolTip(item, project.Name);
            projects.Items.Add(item);
        }
        if (choices.Count == 0)
        {
            projects.Items.Add(new ListViewItem
            {
                Content = new TextBlock { Text = "暂无项目", FontSize = PickerMenu.Dimension("KynxaBodyFontSize"),
                    Foreground = (Brush)Application.Current.Resources["KynxaSecondaryTextBrush"] }, IsEnabled = false
            });
        }
        projects.ItemClick += async (_, args) =>
        {
            // WinUI can return the explicit item's content instead of its container.
            if (args.ClickedItem is not FrameworkElement { Tag: ProjectState project }) return;
            menu.Hide();
            await RunProjectActionAsync(() => { StartWorkspaceProject(project); return Task.CompletedTask; });
        };
        var actions = new StackPanel();
        void AddAction(string label, string glyph, string id, Func<Task> action)
        {
            var button = PickerMenu.Action(label, id, glyph, rowHeight);
            button.Click += (_, _) =>
            {
                menu.Hide();
                // Let the flyout close before presenting the name dialog or native folder picker.
                DispatcherQueue.TryEnqueue(async () => await RunProjectActionAsync(action));
            };
            actions.Children.Add(button);
        }
        AddAction("新建空白项目", "\uE710", "WorkspaceNewProject", async () =>
        {
            if (await CreateBlankProjectAsync() is { } project) StartWorkspaceProject(project);
        });
        AddAction("使用现有文件夹", "\uE8F4", "WorkspaceExistingFolder", async () =>
        {
            if (await PickProjectFolderAsync() is { } project) StartWorkspaceProject(project);
        });
        AddAction("不使用文件夹", "\uE8B7", "WorkspaceWithoutFolder", () =>
        {
            _workWithoutFolder = true;
            UpdateWorkspacePickerVisibility();
            PromptTextBox.Focus(FocusState.Programmatic);
            return Task.CompletedTask;
        });
        menu.Content = PickerMenu.WithFixedFooter(projects, actions);
        menu.ShowAt(WorkspacePickerButton);
    }

    private void StartWorkspaceProject(ProjectState project, bool carryDraft = true)
    {
        ResetCurrentHistorySearch();
        string draft = carryDraft ? PromptTextBox.Text : string.Empty;
        CaptureProjectDraft();
        DiscardEmptyProjectChats();
        var chat = new ProjectChatState { Draft = draft };
        project.Chats.Insert(0, chat);
        _workWithoutFolder = false;
        ShowProjects();
        RenderProjects(project.Id);
        SelectProjectChat(project, chat);
    }

    private ProjectState GetFolderlessWorkspace()
    {
        var workspace = _projects.FirstOrDefault(project => project.IsFolderlessWorkspace);
        if (workspace is not null) return workspace;
        workspace = new ProjectState { Name = "不使用文件夹", IsFolderlessWorkspace = true };
        _projects.Add(workspace);
        return workspace;
    }

    private void RebuildWorkTasks()
    {
        WorkTaskEntries.Clear();
        foreach (var project in _projects.Where(project => project.IsFolderlessWorkspace && !project.IsArchived))
            foreach (var chat in project.Chats.Where(chat => chat.CanPersist && ConversationSearch.MatchesChat(chat, _workHistoryQuery)).OrderByDescending(chat => chat.IsPinned))
                WorkTaskEntries.Add(new ProjectTreeEntry(project, chat));
        bool hasTasks = WorkTaskEntries.Count > 0;
        WorkTaskHistory.Visibility = hasTasks ? Visibility.Visible : Visibility.Collapsed;
        WorkTasksPlaceholder.Visibility = hasTasks ? Visibility.Collapsed : Visibility.Visible;
        WorkTaskHistory.SelectedItem = WorkTaskEntries.FirstOrDefault(task => task.Chat == _activeProjectChat);
        UpdateWorkSidebarHeights(WorkSidebarScrollViewer.ActualHeight);
    }

    private void UpdateWorkSidebarHeights(double available)
    {
        bool hasTasks = WorkTaskEntries.Count > 0;
        WorkSidebarContent.MinHeight = ShellLayoutMetrics.GetWorkSidebarMinimumHeight(hasTasks);
        var heights = ShellLayoutMetrics.GetWorkSidebarHeights(available, hasTasks);
        WorkTaskHistory.MaxHeight = heights.TaskHeight;
        TasksSection.Visibility = heights.TasksVisible ? Visibility.Visible : Visibility.Collapsed;
        ProjectTree.MaxHeight = heights.ProjectHeight;
    }

    private void WorkTaskHistory_ItemClick(object sender, ItemClickEventArgs e)
    {
        if (e.ClickedItem is ProjectTreeEntry { Chat: { } chat } entry) SelectProjectChat(entry.Project, chat);
    }
}
