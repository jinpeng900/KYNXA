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
    private bool _workSidebarLayoutQueued;
    private XamlRoot? _workSidebarViewport;
    public ObservableCollection<ProjectTreeEntry> WorkRecentEntries { get; } = [];
    public ObservableCollection<ProjectTreeEntry> WorkTaskEntries { get; } = [];

    // Selecting a workspace and expanding its tree are independent actions.
    // 选择工作与展开项目树是互相独立的操作。
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
        WorkTaskSearchButton.IsEnabled = project is not null;
        UpdateWorkTaskLanguage();
        UpdateWorkTaskHeaderActions();
        UpdateWorkTaskVisibility();
        RefreshSidebarSearches();
        UpdateWorkRecentVisibility();
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
        RefreshSidebarSearches("recent");
        UpdateWorkSidebarHeights(WorkSidebarContent.ActualHeight);
    }

    private void WorkTaskToggle_Click(object sender, RoutedEventArgs e)
    {
        _layout.WorkTasksExpanded = !_layout.WorkTasksExpanded;
        UpdateWorkTaskVisibility();
        RefreshSidebarSearches("tasks");
        UpdateWorkSidebarHeights(WorkSidebarContent.ActualHeight);
        SaveLayout();
    }

    private void UpdateWorkTaskVisibility()
    {
        WorkTaskHistory.Visibility = _layout.WorkTasksExpanded ? Visibility.Visible : Visibility.Collapsed;
        WorkTaskChevron.Glyph = _layout.WorkTasksExpanded ? "\uE70D" : "\uE76C";
    }

    private void UpdateWorkSidebarHeights(double available)
    {
        if (WorkTaskHistory is null || XamlRoot is null || SidebarPane.ActualHeight <= 0) return;
        if (ViewModel.IsChatMode)
        {
            UpdateChatSidebarHeight();
            return;
        }
        // Read arranged viewport boundaries; summing nominal row heights loses layout-rounding pixels.
        // 读取排版后的真实视口边界；累加名义行高会漏掉布局取整占用的像素。
        double scale = XamlRoot.RasterizationScale;
        double FloorToPixel(double height) => Math.Floor(Math.Max(0, height) * scale) / scale;
        available = SidebarAvailableHeight(WorkSidebarViewport);
        double rowHeight = (double)Application.Current.Resources["KynxaSidebarRowHeight"];
        int projectRows = VisibleProjectRows(VisibleProjectEntries);
        bool projectsExpanded = ProjectTree.Visibility == Visibility.Visible;
        WorkRecentDivider.Visibility = _layout.WorkRecentExpanded && VisibleWorkRecentEntries.Count > 0
            ? Visibility.Visible : Visibility.Collapsed;
        WorkProjectsDivider.Visibility = projectsExpanded && projectRows > 0 ? Visibility.Visible : Visibility.Collapsed;
        double dividerHeight = (WorkRecentDivider.Visibility == Visibility.Visible ? WorkRecentDivider.Height : 0)
            + (WorkProjectsDivider.Visibility == Visibility.Visible ? WorkProjectsDivider.Height : 0);
        double emptyHeight = SidebarHintHeight(WorkRecentSearchEmpty) + SidebarHintHeight(ProjectSearchEmpty)
            + SidebarHintHeight(WorkTaskSearchEmpty);
        int nonemptySections = (_layout.WorkRecentExpanded && VisibleWorkRecentEntries.Count > 0 ? 1 : 0)
            + (projectsExpanded && projectRows > 0 ? 1 : 0) + (_layout.WorkTasksExpanded && VisibleWorkTaskEntries.Count > 0 ? 1 : 0);
        double minimumContentHeight = ShellLayoutMetrics.WorkSidebarHeaderHeight + emptyHeight + dividerHeight + nonemptySections * rowHeight;
        bool outerScroll = available < minimumContentHeight;
        // Very short windows scroll the section shell so each inner list can still show a readable whole row.
        // 极矮窗口滚动分区外壳，让每个内层列表仍能展示可读的完整一行。
        _workSidebarListBudget = Math.Max(available, minimumContentHeight) - ShellLayoutMetrics.WorkSidebarHeaderHeight - emptyHeight - dividerHeight;
        var geometry = WorkSidebarLayout.Calculate(Math.Max(available, minimumContentHeight) - emptyHeight - dividerHeight, _layout.WorkRecentExpanded,
            projectsExpanded, _layout.WorkTasksExpanded, VisibleWorkRecentEntries.Count, projectRows, VisibleWorkTaskEntries.Count, rowHeight,
            _layout.WorkRecentHeightRatio, _layout.WorkProjectsHeightRatio, _layout.WorkTasksHeightRatio)
            ?? new WorkSidebarLayoutResult(0, 0, 0);
        WorkSidebarViewport.Height = available;
        WorkSidebarViewport.MaxHeight = available;
        WorkRecentHistory.Height = FloorToPixel(geometry.RecentHeight);
        ProjectTree.Height = FloorToPixel(geometry.ProjectsHeight);
        WorkTaskHistory.Height = FloorToPixel(geometry.TasksHeight);
        double contentHeight = ShellLayoutMetrics.WorkSidebarHeaderHeight + emptyHeight + dividerHeight + WorkRecentHistory.Height + ProjectTree.Height + WorkTaskHistory.Height;
        WorkSidebarContent.Height = contentHeight;
        WorkSidebarContent.MaxHeight = contentHeight;
        WorkSidebarContent.Margin = outerScroll ? new Thickness(0, 0, 12, 0) : new Thickness(0);
    }

    private double SidebarAvailableHeight(FrameworkElement viewport)
    {
        double top = viewport.TransformToVisual(null).TransformPoint(new Windows.Foundation.Point()).Y;
        double footerTop = SidebarFooter.TransformToVisual(null).TransformPoint(new Windows.Foundation.Point()).Y;
        double footerHeight = SidebarFooter.ActualHeight > 0 ? SidebarFooter.ActualHeight : SidebarPane.RowDefinitions[4].Height.Value;
        double bottom = Math.Min(footerTop, XamlRoot.Size.Height - footerHeight) - RecentArea.Margin.Bottom;
        double scale = XamlRoot.RasterizationScale;
        return Math.Floor(Math.Max(0, bottom - top) * scale) / scale;
    }

    private void UpdateChatSidebarHeight()
    {
        double available = SidebarAvailableHeight(ChatSidebarContent);
        double headerHeight = ChatSidebarContent.RowDefinitions[0].Height.Value + SidebarHintHeight(ChatSearchEmpty);
        double rowHeight = (double)Application.Current.Resources["KynxaSidebarRowHeight"];
        // Limit only complete visible rows; every saved conversation stays in the native scrolling list.
        // 仅限制完整可见行数；全部已保存聊天仍保留在原生滚动列表中。
        int fittingRows = (int)Math.Min(ShellLayoutMetrics.ChatSidebarVisibleRows,
            Math.Floor(Math.Max(0, available - headerHeight) / rowHeight));
        int visibleRows = _layout.ChatHistoryExpanded ? Math.Min(VisibleChatEntries.Count, fittingRows) : 0;
        ChatHistoryList.Height = visibleRows * rowHeight;
        ChatSidebarContent.Height = Math.Min(available, headerHeight + ChatHistoryList.Height);
        ChatSidebarContent.MaxHeight = available;
    }

    private static double SidebarHintHeight(FrameworkElement hint) =>
        hint.Visibility == Visibility.Visible ? hint.Height : 0;

    private void ChatHistoryToggle_Click(object sender, RoutedEventArgs args)
    {
        _layout.ChatHistoryExpanded = !_layout.ChatHistoryExpanded;
        UpdateChatHistoryVisibility();
        RefreshSidebarSearches("chat");
        QueueWorkSidebarLayout();
        SaveLayout();
    }

    private void UpdateChatHistoryVisibility()
    {
        ChatHistoryList.Visibility = _layout.ChatHistoryExpanded ? Visibility.Visible : Visibility.Collapsed;
        ChatHistoryChevron.Glyph = _layout.ChatHistoryExpanded ? "\uE70D" : "\uE76C";
    }

    private void ChatSidebarContent_SizeChanged(object sender, SizeChangedEventArgs args) => QueueWorkSidebarLayout();

    private void AttachWorkSidebarViewport()
    {
        if (ReferenceEquals(_workSidebarViewport, XamlRoot)) return;
        DetachWorkSidebarViewport();
        _workSidebarViewport = XamlRoot;
        if (_workSidebarViewport is not null) _workSidebarViewport.Changed += WorkSidebarViewport_Changed;
    }

    private void DetachWorkSidebarViewport()
    {
        if (_workSidebarViewport is not null) _workSidebarViewport.Changed -= WorkSidebarViewport_Changed;
        _workSidebarViewport = null;
    }

    private void WorkSidebarViewport_Changed(XamlRoot sender, XamlRootChangedEventArgs args) => QueueWorkSidebarLayout();

    private void SidebarPane_SizeChanged(object sender, SizeChangedEventArgs args) => QueueWorkSidebarLayout();

    private static int VisibleProjectRows(IEnumerable<ProjectTreeEntry> entries)
    {
        int count = 0;
        foreach (var entry in entries)
        {
            count++;
            if (entry.IsExpanded) count += VisibleProjectRows(entry.Children);
        }
        return count;
    }

    private void QueueWorkSidebarLayout()
    {
        if (_workSidebarLayoutQueued || _projectViewClosed || _renderingProjects) return;
        // Expansion binding settles after the native event; coalesce it without rebuilding rows.
        // 展开状态绑定在原生事件后完成；合并重排请求，不重建列表行。
        _workSidebarLayoutQueued = DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () =>
        {
            _workSidebarLayoutQueued = false;
            if (!_projectViewClosed && IsLoaded) UpdateWorkSidebarHeights(WorkSidebarContent.ActualHeight);
        });
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
