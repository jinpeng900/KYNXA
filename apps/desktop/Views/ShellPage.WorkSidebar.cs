using System.Collections.ObjectModel;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private Guid? _selectedWorkProjectId;
    private Guid? _workChatToReveal;
    public ObservableCollection<ProjectTreeEntry> WorkRecentEntries { get; } = [];
    public ObservableCollection<ProjectTreeEntry> WorkTaskEntries { get; } = [];

    // Selecting a workspace and expanding its tree are independent actions.
    private void SelectWorkspaceProject(ProjectState project)
    {
        if (!_projectsReady || project.IsArchived || project.IsFolderlessWorkspace) return;
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
        WorkSidebarState.ReconcileChats(WorkTaskEntries, WorkSidebarState.ProjectChats(project), _activeProjectChat?.Id, replying);
        WorkTaskProjectLabel.Text = project?.Name ?? string.Empty;
        WorkTaskProjectLabel.Visibility = project is null ? Visibility.Collapsed : Visibility.Visible;
        AutomationProperties.SetName(WorkTaskHistory, project is null ? "当前项目的聊天" : $"{project.Name}的聊天");
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
        AutomationProperties.SetName(WorkRecentToggle, _layout.WorkRecentExpanded ? "收起最近的工作聊天" : "展开最近的工作聊天");
        UpdateWorkSidebarHeights(WorkSidebarContent.ActualHeight);
    }

    private void UpdateWorkSidebarHeights(double available)
    {
        if (!double.IsFinite(available) || available <= 0) return;
        // Recent and projects scroll independently and leave at least half the
        // sidebar for tasks. Their limits follow window height, not fixed pixels.
        double navigationBudget = Math.Max(0, available * 0.5 - 76);
        bool recent = _layout.WorkRecentExpanded;
        bool projects = ProjectTree.Visibility == Visibility.Visible;
        WorkRecentHistory.MaxHeight = recent && projects ? navigationBudget * 0.5 : navigationBudget;
        ProjectTree.MaxHeight = recent && projects ? navigationBudget * 0.5 : navigationBudget;
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
