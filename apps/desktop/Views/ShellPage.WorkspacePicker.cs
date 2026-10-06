using KYNXA_Desktop.Services;
using KYNXA_Desktop.Models.UI;
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

    private static string WorkChatTitle(ProjectState project, ProjectChatState chat) =>
        project.IsFolderlessWorkspace ? chat.Title : $"{project.Name} / {chat.Title}";

    private void UpdateWorkspacePickerVisibility()
    {
        ComposerHost.IsFooterVisible = !ViewModel.IsChatMode && _activeProjectChat is null;
        WorkspacePickerButton.Visibility = ComposerHost.IsFooterVisible ? Visibility.Visible : Visibility.Collapsed;
        WorkspacePickerButton.IsEnabled = _projectsReady;
        var project = KYNXA_Desktop.Services.WorkSidebarState.FindSelectedProject(_projects, _selectedWorkProjectId);
        WorkspacePickerLabel.Text = project?.Name ?? (_workWithoutFolder ? UiText.Get("不使用文件夹") : UiText.Get("选择项目"));
        AutomationProperties.SetName(WorkspacePickerButton, project is not null ? string.Format(UiText.Get("工作区：{0}"), project.Name) :
            _workWithoutFolder ? UiText.Get("工作区：不使用文件夹") : UiText.Get("选择工作区"));
    }

    private void WorkspacePickerButton_Click(object sender, RoutedEventArgs e) => ShowWorkspacePicker();

    private void ShowWorkspacePicker()
    {
        if (!_projectsReady || ViewModel.IsChatMode || _activeProjectChat is not null) return;
        var menu = PickerMenu.Create(FlyoutPlacementMode.BottomEdgeAlignedLeft, "KynxaWorkspaceFlyoutPresenterStyle");
        var choices = _projects.Where(project => !project.IsArchived && !project.IsFolderlessWorkspace)
            .OrderByDescending(project => project.IsPinned).ToList();
        var projects = PickerMenu.CreateList("WorkspaceProjectList", UiText.Get("已有项目"), "KynxaWorkspaceListItemStyle", ListViewSelectionMode.None);
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
                Content = new TextBlock { Text = UiText.Get("暂无项目"), FontSize = PickerMenu.Dimension("KynxaBodyFontSize"),
                    Foreground = (Brush)Application.Current.Resources["KynxaSecondaryTextBrush"] }, IsEnabled = false
            });
        }
        projects.ItemClick += async (_, args) =>
        {
            // WinUI can return the explicit item's content instead of its container.
            // WinUI 可能返回选项的内容，而不是其容器。
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
                // 先让浮出菜单关闭，再显示名称对话框或原生文件夹选择器。
                DispatcherQueue.TryEnqueue(async () => await RunProjectActionAsync(action));
            };
            actions.Children.Add(button);
        }
        AddAction(UiText.Get("新建空白项目"), "\uE710", "WorkspaceNewProject", async () =>
        {
            if (await CreateBlankProjectAsync() is { } project) StartWorkspaceProject(project);
        });
        AddAction(UiText.Get("使用现有文件夹"), "\uE8F4", "WorkspaceExistingFolder", async () =>
        {
            if (await PickProjectFolderAsync() is { } project) StartWorkspaceProject(project);
        });
        AddAction(UiText.Get("不使用文件夹"), "\uE8B7", "WorkspaceWithoutFolder", () =>
        {
            _selectedWorkProjectId = null;
            _workWithoutFolder = true;
            _workConversationTitle = string.Empty;
            RenderProjects();
            UpdateConversationTitle();
            UpdateWorkspacePickerVisibility();
            PromptTextBox.Focus(FocusState.Programmatic);
            return Task.CompletedTask;
        });
        menu.Content = PickerMenu.WithFixedFooter(projects, actions);
        ((FrameworkElement)menu.Content).Width = PickerMenu.SetContentWidth(menu, 250, XamlRoot.Size.Width);
        menu.ShowAt(WorkspacePickerButton);
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

}
