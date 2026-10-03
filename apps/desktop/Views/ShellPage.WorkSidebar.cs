using System.Collections.ObjectModel;
using KYNXA_Desktop.Layout;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private Guid? _selectedWorkProjectId;
    private Guid? _workChatToReveal;
    private bool _workTaskHeaderHovered;
    private readonly WorkSidebarLayout _workSidebarLayout = new();
    public ObservableCollection<ProjectTreeEntry> WorkRecentEntries { get; } = [];
    public ObservableCollection<ProjectTreeEntry> WorkTaskEntries { get; } = [];

    // Selecting a workspace and expanding its tree are independent actions.
    private void SelectWorkspaceProject(ProjectState project)
    {
        if (!_projectsReady || project.IsArchived || project.IsFolderlessWorkspace) return;
        if (_selectedWorkProjectId == project.Id && _activeProjectChat is { CanPersist: false } draft && project.Chats.Contains(draft))
        {
            SelectProjectChat(project, draft);
            return;
        }
        CaptureProjectDraft();
        DiscardEmptyProjectChats();
        _activeProjectChat = null;
        _selectedWorkProjectId = project.Id;
        _workWithoutFolder = false;
        _workDraft = string.Empty;
        _workConversationTitle = project.Name;
        SetPrimaryMode(false, updateConversation: false);
        PromptTextBox.Text = ViewModel.Prompt = string.Empty;
        RenderProjects();
        UpdateConversationTitle();
        UpdateConversationPresentation();
        PromptTextBox.Focus(FocusState.Programmatic);
    }

    private void RebuildWorkTasks()
    {
        var project = WorkSidebarState.FindSelectedProject(_projects, _selectedWorkProjectId);
        if (project is null) _selectedWorkProjectId = null;
        var replying = _pendingReplies.Keys.ToHashSet();
        WorkSidebarState.ReconcileChats(WorkRecentEntries, WorkSidebarState.RecentChats(_projects, _layout.RecentWorkChatIds), _activeProjectChat?.Id, replying);
        WorkSidebarState.ReconcileChats(WorkTaskEntries, WorkSidebarState.ProjectChats(project, _activeProjectChat?.Id), _activeProjectChat?.Id, replying);
        WorkTaskProjectLabel.Text = project?.Name ?? string.Empty;
        WorkTaskProjectLabel.Visibility = project is null ? Visibility.Collapsed : Visibility.Visible;
        WorkTaskAddChatButton.Visibility = project is null ? Visibility.Collapsed : Visibility.Visible;
        WorkTaskAddChatButton.IsEnabled = project is not null;
        UpdateWorkTaskLanguage();
        UpdateWorkTaskHeaderActions();
        WorkRecentCount.Text = WorkRecentEntries.Count.ToString();
        WorkRecentCount.Visibility = WorkRecentEntries.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
        UpdateWorkRecentVisibility();
        UpdateWorkSidebarHeights(WorkSidebarContent.ActualHeight);
        UpdateWorkspacePickerVisibility();
        if (_workChatToReveal is Guid chatId)
        {
            _workChatToReveal = null;
            DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () =>
            {
                if (_projectViewClosed || !IsLoaded || _activeProjectChat?.Id != chatId) return;
                var task = WorkTaskEntries.FirstOrDefault(entry => entry.Chat?.Id == chatId);
                if (task is not null && WorkTaskHistory.IsLoaded) WorkTaskHistory.ScrollIntoView(task);
                var recent = WorkRecentEntries.FirstOrDefault(entry => entry.Chat?.Id == chatId);
                if (recent is not null && _layout.WorkRecentExpanded && WorkRecentHistory.IsLoaded)
                    WorkRecentHistory.ScrollIntoView(recent);
            });
        }
    }

    private async void WorkTaskAddChat_Click(object sender, RoutedEventArgs e)
    {
        var project = WorkSidebarState.FindSelectedProject(_projects, _selectedWorkProjectId);
        if (project is null) return;
        await RunProjectActionAsync(() =>
        {
            StartNewProjectChat(project);
            return Task.CompletedTask;
        });
    }

    private void UpdateWorkTaskLanguage()
    {
        var project = WorkSidebarState.FindSelectedProject(_projects, _selectedWorkProjectId);
        AutomationProperties.SetName(WorkTaskAddChatButton, project is null ? UiText.Get("在当前项目中添加聊天") : string.Format(UiText.Get("在{0}中添加聊天"), project.Name));
        AutomationProperties.SetName(WorkTaskHistory, project is null ? UiText.Get("当前项目的聊天") : string.Format(UiText.Get("{0}的聊天"), project.Name));
    }

    private void WorkTaskHeader_PointerEntered(object sender, PointerRoutedEventArgs e)
    {
        _workTaskHeaderHovered = true;
        UpdateWorkTaskHeaderActions();
    }

    private void WorkTaskHeader_PointerExited(object sender, PointerRoutedEventArgs e)
    {
        _workTaskHeaderHovered = false;
        UpdateWorkTaskHeaderActions();
    }

    private void WorkTaskHeader_FocusChanged(object sender, RoutedEventArgs e) => UpdateWorkTaskHeaderActions();

    private void UpdateWorkTaskHeaderActions()
    {
        if (WorkTaskAddChatButton is null || WorkTaskHeader is null) return;
        bool visible = WorkTaskAddChatButton.IsEnabled && (_workTaskHeaderHovered || ContainsKeyboardFocus(WorkTaskHeader));
        WorkTaskAddChatButton.Opacity = visible ? 1 : 0;
        WorkTaskAddChatButton.IsHitTestVisible = visible;
    }

    private void WorkRecentToggle_Click(object sender, RoutedEventArgs e)
    {
        _layout.WorkRecentExpanded = !_layout.WorkRecentExpanded;
        UpdateWorkRecentVisibility();
        SaveLayout();
    }

    private void UpdateWorkRecentVisibility()
    {
        WorkRecentHistory.Visibility = _layout.WorkRecentExpanded ? Visibility.Visible : Visibility.Collapsed;
        WorkRecentChevron.Glyph = _layout.WorkRecentExpanded ? "\uE70D" : "\uE76C";
        AutomationProperties.SetName(WorkRecentToggle, _layout.WorkRecentExpanded ? UiText.Get("收起最近的工作聊天") : UiText.Get("展开最近的工作聊天"));
        UpdateWorkSidebarHeights(WorkSidebarContent.ActualHeight);
    }

    private void UpdateWorkSidebarHeights(double available)
    {
        var geometry = WorkSidebarLayout.Calculate(available, _layout.WorkRecentExpanded,
            ProjectTree.Visibility == Visibility.Visible, _layout.WorkNavigationRatio, _layout.WorkRecentRatio);
        if (geometry is null) return;
        WorkNavigationGrip.Visibility = geometry.NavigationGripHeight > 0 ? Visibility.Visible : Visibility.Collapsed;
        WorkNavigationGripRow.Height = new GridLength(geometry.NavigationGripHeight);
        WorkRecentGrip.Visibility = geometry.RecentGripHeight > 0 ? Visibility.Visible : Visibility.Collapsed;
        WorkRecentGripRow.Height = new GridLength(geometry.RecentGripHeight);
        WorkNavigationRow.Height = SidebarGridLength(geometry.NavigationRow);
        WorkRecentRow.Height = SidebarGridLength(geometry.RecentRow);
        WorkProjectsRow.Height = SidebarGridLength(geometry.ProjectsRow);
        WorkRecentHistory.MaxHeight = geometry.RecentMaximumHeight;
        ProjectTree.MaxHeight = geometry.ProjectsMaximumHeight;
    }

    private static GridLength SidebarGridLength(WorkSidebarRowHeight height) => height.Sizing switch
    {
        WorkSidebarRowSizing.Pixels => new GridLength(height.Value),
        WorkSidebarRowSizing.Star => new GridLength(height.Value, GridUnitType.Star),
        _ => GridLength.Auto
    };

    private void WorkNavigationGrip_DragStarted(object? sender, EventArgs e) =>
        _workSidebarLayout.BeginNavigationDrag(WorkNavigationRow.ActualHeight, _layout);

    private void WorkNavigationGrip_DragDelta(object? sender, Controls.ResizeDeltaEventArgs e)
    {
        double available = WorkSidebarContent.ActualHeight;
        if (_workSidebarLayout.ResizeNavigation(_layout, available, _layout.WorkRecentExpanded,
            ProjectTree.Visibility == Visibility.Visible, e.Delta)) UpdateWorkSidebarHeights(available);
    }

    private void WorkNavigationGrip_CancelRequested(object? sender, EventArgs e)
    {
        _workSidebarLayout.CancelNavigationDrag(_layout);
        UpdateWorkSidebarHeights(WorkSidebarContent.ActualHeight);
    }

    private void WorkNavigationGrip_ResetRequested(object? sender, EventArgs e)
    {
        _workSidebarLayout.ResetNavigation(_layout);
        UpdateWorkSidebarHeights(WorkSidebarContent.ActualHeight);
        SaveLayout();
    }

    private void WorkRecentGrip_DragStarted(object? sender, EventArgs e) =>
        _workSidebarLayout.BeginRecentDrag(WorkRecentRow.ActualHeight, WorkNavigationRow.ActualHeight, _layout);

    private void WorkRecentGrip_DragDelta(object? sender, Controls.ResizeDeltaEventArgs e)
    {
        if (_workSidebarLayout.ResizeRecent(_layout, WorkSidebarContent.ActualHeight, e.Delta))
            UpdateWorkSidebarHeights(WorkSidebarContent.ActualHeight);
    }

    private void WorkRecentGrip_CancelRequested(object? sender, EventArgs e)
    {
        _workSidebarLayout.CancelRecentDrag(_layout);
        UpdateWorkSidebarHeights(WorkSidebarContent.ActualHeight);
    }

    private void WorkRecentGrip_ResetRequested(object? sender, EventArgs e)
    {
        _workSidebarLayout.ResetRecent(_layout);
        UpdateWorkSidebarHeights(WorkSidebarContent.ActualHeight);
        SaveLayout();
    }

    private void RecordWorkChatActivity(ProjectChatState chat)
    {
        if (!chat.CanPersist || chat.IsArchived) return;
        var ids = _projects.Where(project => !project.IsArchived).SelectMany(project => project.Chats)
            .Where(saved => saved.CanPersist && !saved.IsArchived).Select(saved => saved.Id).ToHashSet();
        _layout.RecentWorkChatIds.RemoveAll(id => id == chat.Id || !ids.Contains(id));
        _layout.RecentWorkChatIds.Insert(0, chat.Id);
        SaveLayout();
    }

    private void WorkChatHistory_ItemClick(object sender, ItemClickEventArgs e)
    {
        if (e.ClickedItem is ProjectTreeEntry { Chat: { } chat } entry)
            SelectProjectChat(entry.Project, chat);
    }
}
