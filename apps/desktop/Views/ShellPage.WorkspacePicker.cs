using System.Collections.ObjectModel;
using KYNXA_Desktop.Models.UI;
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
    private double WorkspacePickerHeight => WorkspacePickerButton.Visibility == Visibility.Visible ? 36 : 0;

    private static string WorkChatTitle(ProjectState project, ProjectChatState chat) =>
        project.IsFolderlessWorkspace ? chat.Title : $"{project.Name} / {chat.Title}";

    private void UpdateWorkspacePickerVisibility()
    {
        WorkspacePickerButton.Visibility = !ViewModel.IsChatMode && _activeProjectChat is null
            ? Visibility.Visible : Visibility.Collapsed;
        WorkspacePickerButton.IsEnabled = _projectsReady;
        WorkspacePickerLabel.Text = _workWithoutFolder ? "不使用文件夹" : "选择项目";
        AutomationProperties.SetName(WorkspacePickerButton, _workWithoutFolder ? "工作区：不使用文件夹" : "选择工作区");
    }

    private void WorkspacePickerButton_Click(object sender, RoutedEventArgs e) => ShowWorkspacePicker();

    private void ShowWorkspacePicker()
    {
        if (!_projectsReady || ViewModel.IsChatMode || _activeProjectChat is not null) return;
        var menu = new Flyout
        {
            Placement = FlyoutPlacementMode.BottomEdgeAlignedLeft,
            FlyoutPresenterStyle = (Style)Application.Current.Resources["KynxaWorkspaceFlyoutPresenterStyle"]
        };
        var choices = _projects.Where(project => !project.IsArchived && !project.IsFolderlessWorkspace)
            .OrderByDescending(project => project.IsPinned).ToList();
        var content = new Grid { Width = 250 };
        content.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        content.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        content.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        // Only the project list scrolls. Actions remain visible below its fixed maximum height.
        var projects = new ListView
        {
            Height = Math.Min(Math.Max(72, Math.Min(216, XamlRoot.Size.Height - 200)), Math.Max(36, choices.Count * 36)),
            Padding = new Thickness(0), SelectionMode = ListViewSelectionMode.None, IsItemClickEnabled = true,
            ItemContainerStyle = (Style)Application.Current.Resources["KynxaWorkspaceListItemStyle"]
        };
        projects.Resources["ListViewItemSelectionIndicatorVisualEnabled"] = false;
        ScrollViewer.SetHorizontalScrollMode(projects, ScrollMode.Disabled);
        ScrollViewer.SetHorizontalScrollBarVisibility(projects, ScrollBarVisibility.Disabled);
        ScrollViewer.SetVerticalScrollBarVisibility(projects, ScrollBarVisibility.Auto);
        AutomationProperties.SetAutomationId(projects, "WorkspaceProjectList");
        AutomationProperties.SetName(projects, "已有项目");
        foreach (var project in choices)
        {
            var row = WorkspaceMenuRow(project.Name, "\uE8B7");
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
                Content = new TextBlock { Text = "暂无项目", FontSize = 14,
                    Foreground = (Brush)Application.Current.Resources["KynxaSecondaryTextBrush"] }, IsEnabled = false
            });
        }
        projects.ItemClick += async (_, args) =>
        {
            if (args.ClickedItem is not ListViewItem { Tag: ProjectState project }) return;
            menu.Hide();
            await RunProjectActionAsync(() => { StartWorkspaceProject(project); return Task.CompletedTask; });
        };
        content.Children.Add(projects);
        var separator = new Border
        {
            Height = 1, Margin = new Thickness(8, 5, 8, 5),
            Background = (Brush)Application.Current.Resources["KynxaDividerBrush"]
        };
        Grid.SetRow(separator, 1);
        content.Children.Add(separator);
        var actions = new StackPanel();
        void AddAction(string label, string glyph, string id, Func<Task> action)
        {
            var button = new Button
            {
                Content = WorkspaceMenuRow(label, glyph), Height = 36,
                Style = (Style)Application.Current.Resources["KynxaModelFooterButtonStyle"]
            };
            AutomationProperties.SetAutomationId(button, id);
            AutomationProperties.SetName(button, label);
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
        Grid.SetRow(actions, 2);
        content.Children.Add(actions);
        menu.Content = content;
        menu.ShowAt(WorkspacePickerButton);
    }

    private static Grid WorkspaceMenuRow(string label, string glyph)
    {
        var row = new Grid { ColumnSpacing = 8 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(18) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.Children.Add(new FontIcon { Glyph = glyph, FontSize = 16, VerticalAlignment = VerticalAlignment.Center });
        var text = new TextBlock { Text = label, FontSize = 14, VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis };
        Grid.SetColumn(text, 1);
        row.Children.Add(text);
        return row;
    }

    private void StartWorkspaceProject(ProjectState project)
    {
        string draft = PromptTextBox.Text;
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
            foreach (var chat in project.Chats.Where(chat => chat.CanPersist && !chat.IsArchived).OrderByDescending(chat => chat.IsPinned))
                WorkTaskEntries.Add(new ProjectTreeEntry(project, chat));
        bool hasTasks = WorkTaskEntries.Count > 0;
        WorkTaskHistory.Visibility = hasTasks ? Visibility.Visible : Visibility.Collapsed;
        WorkTasksPlaceholder.Visibility = hasTasks ? Visibility.Collapsed : Visibility.Visible;
        WorkTaskHistory.SelectedItem = WorkTaskEntries.FirstOrDefault(task => task.Chat == _activeProjectChat);
        UpdateWorkSidebarHeights(WorkSidebarContent.ActualHeight);
    }

    private void UpdateWorkSidebarHeights(double available)
    {
        double taskHeight = Math.Max(0, Math.Min(160, available * 0.25));
        WorkTaskHistory.MaxHeight = taskHeight;
        double reserved = WorkTaskEntries.Count > 0 ? taskHeight + 96 : 136;
        ProjectTree.MaxHeight = Math.Max(0, Math.Min(320, Math.Min(available * 0.65, available - reserved)));
    }

    private void WorkTaskHistory_ItemClick(object sender, ItemClickEventArgs e)
    {
        if (e.ClickedItem is ProjectTreeEntry { Chat: { } chat } entry) SelectProjectChat(entry.Project, chat);
    }
}
