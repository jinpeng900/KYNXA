using System.Collections.ObjectModel;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.System;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private sealed class SidebarSearchState
    {
        public bool IsOpen;
        public bool Composing;
        public bool SuppressEnter;
        public string AppliedQuery = string.Empty;
    }

    private static readonly string[] SidebarWorkSearchScopes = ["recent", "projects", "tasks"];
    private readonly Dictionary<string, SidebarSearchState> _sidebarSearchStates = new(StringComparer.Ordinal);
    private bool _updatingSidebarSearch;
    private bool _sidebarSearchProjectInitialized;
    private Guid? _sidebarSearchProjectId;

    public ObservableCollection<ProjectTreeEntry> VisibleWorkRecentEntries { get; } = [];
    public ObservableCollection<ProjectTreeEntry> VisibleProjectEntries { get; } = [];
    public ObservableCollection<ProjectTreeEntry> VisibleWorkTaskEntries { get; } = [];
    internal ObservableCollection<RecentConversation> VisibleChatEntries { get; } = [];

    private static bool IsSidebarSearchScope(string? scope) => scope is "recent" or "projects" or "tasks" or "chat";

    private SidebarSearchState SidebarSearchStateFor(string scope)
    {
        if (!_sidebarSearchStates.TryGetValue(scope, out var state))
            _sidebarSearchStates.Add(scope, state = new SidebarSearchState());
        return state;
    }

    private (TextBox Box, Button Button, FrameworkElement Title, FontIcon Glyph, TextBlock Empty) SidebarSearchControls(string scope) => scope switch
    {
        "recent" => (WorkRecentSearchBox, WorkRecentSearchButton, WorkRecentTitleHost, WorkRecentSearchGlyph, WorkRecentSearchEmpty),
        "projects" => (ProjectSearchBox, ProjectSearchButton, ProjectTitleHost, ProjectSearchGlyph, ProjectSearchEmpty),
        "tasks" => (WorkTaskSearchBox, WorkTaskSearchButton, WorkTaskTitleHost, WorkTaskSearchGlyph, WorkTaskSearchEmpty),
        "chat" => (ChatSearchBox, ChatSearchButton, ChatTitleHost, ChatSearchGlyph, ChatSearchEmpty),
        _ => throw new ArgumentOutOfRangeException(nameof(scope))
    };

    private static string SidebarSearchLabel(string scope) => scope switch
    {
        "recent" => "搜索工作任务",
        "projects" => "搜索项目",
        "tasks" => "搜索当前项目任务",
        _ => "搜索聊天"
    };

    private static string? SidebarSearchScope(object sender) => sender is FrameworkElement { Tag: string scope }
        && IsSidebarSearchScope(scope) ? scope : null;

    private void SidebarSearch_Click(object sender, RoutedEventArgs args)
    {
        if (SidebarSearchScope(sender) is not { } scope) return;
        if (SidebarSearchStateFor(scope).IsOpen) CloseSidebarSearch(scope);
        else OpenSidebarSearch(scope);
    }

    private void OpenSidebarSearch(string scope)
    {
        if (!IsSidebarSearchScope(scope) || !_projectsReady || _chatClosing || HasPresentationModal()
            || scope == "tasks" && WorkSidebarState.FindSelectedProject(_projects, _selectedWorkProjectId) is null) return;
        if (_isCompactLayout) _temporarySidebarOpen = true;
        else if (_layout.SidebarCollapsed) SidebarToggle_Click(this, new RoutedEventArgs());
        var state = SidebarSearchStateFor(scope);
        state.IsOpen = true;
        bool changed = false;
        if (scope == "recent" && !_layout.WorkRecentExpanded)
        {
            _layout.WorkRecentExpanded = true;
            UpdateWorkRecentVisibility();
            changed = true;
        }
        else if (scope == "projects")
        {
            ProjectTree.Visibility = Visibility.Visible;
            ProjectsChevron.Glyph = "\uE70D";
        }
        else if (scope == "tasks" && !_layout.WorkTasksExpanded)
        {
            _layout.WorkTasksExpanded = true;
            UpdateWorkTaskVisibility();
            changed = true;
        }
        else if (scope == "chat" && !_layout.ChatHistoryExpanded)
        {
            _layout.ChatHistoryExpanded = true;
            UpdateChatHistoryVisibility();
            changed = true;
        }
        UpdateSidebarSearchPresentation(scope);
        RefreshSidebarSearches(scope);
        ApplyLayout();
        if (changed) SaveLayout();
        var box = SidebarSearchControls(scope).Box;
        box.StartBringIntoView();
        if (!box.Focus(FocusState.Programmatic))
            DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () =>
            {
                if (!_chatClosing && state.IsOpen && box.IsLoaded)
                {
                    box.StartBringIntoView();
                    box.Focus(FocusState.Programmatic);
                }
            });
    }

    private void CloseSidebarSearch(string scope)
    {
        if (!IsSidebarSearchScope(scope)) return;
        var state = SidebarSearchStateFor(scope);
        state.IsOpen = state.Composing = state.SuppressEnter = false;
        SetSidebarSearchText(scope, string.Empty);
        UpdateSidebarSearchPresentation(scope);
        SidebarSearchControls(scope).Empty.Visibility = Visibility.Collapsed;
        RefreshSidebarSearches(scope);
    }

    private void SetSidebarSearchText(string scope, string text)
    {
        bool wasUpdating = _updatingSidebarSearch;
        _updatingSidebarSearch = true;
        try { SidebarSearchControls(scope).Box.Text = text; }
        finally { _updatingSidebarSearch = wasUpdating; }
    }

    private void UpdateSidebarSearchPresentation(string scope)
    {
        var controls = SidebarSearchControls(scope);
        bool open = SidebarSearchStateFor(scope).IsOpen;
        Button toggle = scope switch
        {
            "recent" => WorkRecentToggle,
            "projects" => ProjectsToggle,
            "tasks" => WorkTaskToggle,
            _ => ChatHistoryToggle
        };
        // A search editor owns the title column; only the leading arrow remains a collapse target.
        // 搜索框独占标题列，展开／收起按钮仅保留前侧箭头的命中区。
        Grid.SetColumnSpan(toggle, open ? 1 : 2);
        controls.Box.Visibility = open ? Visibility.Visible : Visibility.Collapsed;
        controls.Title.Visibility = open ? Visibility.Collapsed : Visibility.Visible;
        controls.Glyph.Glyph = open ? "\uE72B" : "\uE721";
        string label = open ? "结束搜索" : SidebarSearchLabel(scope);
        UiLocalization.Bind(controls.Button, ToolTipService.ToolTipProperty, label);
        UiLocalization.Bind(controls.Button, AutomationProperties.NameProperty, label);
    }

    private void SidebarSearch_TextChanged(object sender, TextChangedEventArgs args)
    {
        if (_updatingSidebarSearch || SidebarSearchScope(sender) is not { } scope || SidebarSearchStateFor(scope).Composing) return;
        RefreshSidebarSearches(scope);
    }

    private void SidebarSearch_CompositionStarted(TextBox sender, TextCompositionStartedEventArgs args)
    {
        if (SidebarSearchScope(sender) is not { } scope) return;
        var state = SidebarSearchStateFor(scope);
        state.Composing = state.SuppressEnter = true;
    }

    private void SidebarSearch_CompositionEnded(TextBox sender, TextCompositionEndedEventArgs args)
    {
        if (SidebarSearchScope(sender) is not { } scope) return;
        var state = SidebarSearchStateFor(scope);
        state.Composing = false;
        // The Enter that accepts an IME candidate must not also activate a conversation.
        // 确认输入法候选字的 Enter 不能同时打开会话。
        state.SuppressEnter = true;
        RefreshSidebarSearches(scope);
    }

    private void SidebarSearch_KeyUp(object sender, KeyRoutedEventArgs args)
    {
        if (SidebarSearchScope(sender) is { } scope && !SidebarSearchStateFor(scope).Composing)
            SidebarSearchStateFor(scope).SuppressEnter = false;
    }

    private void SidebarSearch_KeyDown(object sender, KeyRoutedEventArgs args)
    {
        if (SidebarSearchScope(sender) is not { } scope) return;
        var state = SidebarSearchStateFor(scope);
        if (state.Composing) return;
        if (args.Key == VirtualKey.Escape)
        {
            CloseSidebarSearch(scope);
            SidebarSearchControls(scope).Button.Focus(FocusState.Keyboard);
            args.Handled = true;
        }
        else if (args.Key == VirtualKey.Down)
        {
            FocusFirstSidebarSearchResult(scope);
            args.Handled = true;
        }
        else if (args.Key == VirtualKey.Enter)
        {
            if (!state.SuppressEnter) ActivateSidebarSearchResult(scope);
            args.Handled = true;
        }
    }

    private void SidebarSearchResults_KeyDown(object sender, KeyRoutedEventArgs args)
    {
        if (args.Key != VirtualKey.Escape) return;
        string? scope = ReferenceEquals(sender, WorkRecentHistory) ? "recent" : ReferenceEquals(sender, ProjectTree) ? "projects"
            : ReferenceEquals(sender, WorkTaskHistory) ? "tasks" : ReferenceEquals(sender, ChatHistoryList) ? "chat" : null;
        if (scope is null || !SidebarSearchStateFor(scope).IsOpen) return;
        CloseSidebarSearch(scope);
        SidebarSearchControls(scope).Button.Focus(FocusState.Keyboard);
        args.Handled = true;
    }

    private bool? HandleFocusedSidebarSearchEscape()
    {
        foreach (string scope in new[] { "recent", "projects", "tasks", "chat" })
        {
            var state = SidebarSearchStateFor(scope);
            if (!state.IsOpen) continue;
            var controls = SidebarSearchControls(scope);
            FrameworkElement results = scope == "recent" ? WorkRecentHistory : scope == "projects" ? ProjectTree
                : scope == "tasks" ? WorkTaskHistory : ChatHistoryList;
            if (!ContainsKeyboardFocus(controls.Box) && !ContainsKeyboardFocus(results) && !ContainsKeyboardFocus(controls.Button)) continue;
            // Let the input method consume candidate Escape before changing the surrounding sidebar.
            // 输入法候选状态先接收 Escape，不改变外围侧栏。
            if (state.Composing) return false;
            CloseSidebarSearch(scope);
            controls.Button.Focus(FocusState.Keyboard);
            return true;
        }
        return null;
    }

    private void RefreshSidebarSearches(string? scope = null)
    {
        if (scope is not null && !IsSidebarSearchScope(scope)) return;
        if (scope != "chat") ResetSidebarTaskQueryForProjectChange();
        if (scope == "chat") RefreshSidebarSearchScope("chat");
        else if (scope is null)
            foreach (string workScope in SidebarWorkSearchScopes) RefreshSidebarSearchScope(workScope);
        else RefreshSidebarSearchScope(scope);
        QueueWorkSidebarLayout();
    }

    private void ResetSidebarTaskQueryForProjectChange()
    {
        bool changed = _sidebarSearchProjectInitialized && _sidebarSearchProjectId != _selectedWorkProjectId;
        _sidebarSearchProjectInitialized = true;
        _sidebarSearchProjectId = _selectedWorkProjectId;
        if (!changed) return;
        var state = SidebarSearchStateFor("tasks");
        state.AppliedQuery = string.Empty;
        state.Composing = state.SuppressEnter = false;
        SetSidebarSearchText("tasks", string.Empty);
    }

    private void RefreshSidebarSearchScope(string scope)
    {
        var state = SidebarSearchStateFor(scope);
        var controls = SidebarSearchControls(scope);
        string query = state.Composing ? state.AppliedQuery : controls.Box.Text.Trim();
        bool queryChanged = state.AppliedQuery != query;
        state.AppliedQuery = query;
        if (scope == "projects" && queryChanged)
        {
            // Refresh full tree rows before changing its projection; filtering never changes formal ownership.
            // 修改树投影前先刷新完整树行；过滤不改变正式归属关系。
            RenderProjects();
            return;
        }
        string[] terms = query.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
        bool Matches(string title, string? project = null) => terms.All(term => title.Contains(term, StringComparison.OrdinalIgnoreCase)
            || project?.Contains(term, StringComparison.OrdinalIgnoreCase) == true);
        int count;
        bool expanded;
        if (scope == "chat")
        {
            IReadOnlyList<RecentConversation> desired = terms.Length == 0 ? ViewModel.RecentConversations
                : ViewModel.RecentConversations.Where(row => Matches(row.Title)).ToArray();
            ReconcileSidebarChats(VisibleChatEntries, desired);
            ChatHistoryList.SelectedItem = VisibleChatEntries.FirstOrDefault(row => row.Id == _activeStandaloneChat?.Id);
            count = VisibleChatEntries.Count;
            expanded = _layout.ChatHistoryExpanded;
            SetSidebarSearchCount(ChatHistoryCount, count);
        }
        else
        {
            if (scope == "tasks")
                controls.Button.IsEnabled = state.IsOpen || WorkSidebarState.FindSelectedProject(_projects, _selectedWorkProjectId) is not null;
            var source = scope == "recent" ? WorkRecentEntries : scope == "projects" ? ProjectEntries : WorkTaskEntries;
            var target = scope == "recent" ? VisibleWorkRecentEntries : scope == "projects" ? VisibleProjectEntries : VisibleWorkTaskEntries;
            IReadOnlyList<ProjectTreeEntry> desired = terms.Length == 0 ? source
                : source.Where(row => Matches(scope == "projects" ? row.Project.Name : row.Title,
                    scope == "recent" ? row.Project.Name : null)).ToArray();
            if (!SameSidebarRows(target, desired))
            {
                bool wasRendering = _renderingProjects;
                _renderingProjects = true;
                var expansions = scope == "projects" ? ProjectEntries.Select(row => (Row: row, row.IsExpanded)).ToArray() : [];
                try { ProjectTreeReconciler.ReconcileRows(target, desired); }
                finally
                {
                    // Native tree unbinding may write IsExpanded; preserve the original UI state during filtering.
                    // 原生树解绑可能回写 IsExpanded；过滤期间保留原有界面展开状态。
                    foreach (var expansion in expansions) expansion.Row.IsExpanded = expansion.IsExpanded;
                    _renderingProjects = wasRendering;
                }
            }
            count = target.Count;
            expanded = scope == "recent" ? _layout.WorkRecentExpanded : scope == "projects"
                ? ProjectTree.Visibility == Visibility.Visible : _layout.WorkTasksExpanded;
            SetSidebarSearchCount(scope == "recent" ? WorkRecentCount : scope == "projects" ? WorkProjectCount : WorkTaskCount, count);
        }
        controls.Empty.Visibility = query.Length > 0 && count == 0 && expanded ? Visibility.Visible : Visibility.Collapsed;
    }

    private static bool SameSidebarRows<T>(IReadOnlyList<T> current, IReadOnlyList<T> desired) where T : class =>
        current.Count == desired.Count && current.Select((row, index) => ReferenceEquals(row, desired[index])).All(same => same);

    private static void ReconcileSidebarChats(ObservableCollection<RecentConversation> rows, IReadOnlyList<RecentConversation> desired)
    {
        if (SameSidebarRows(rows, desired)) return;
        var retainedIds = desired.Select(row => row.Id).ToHashSet();
        for (int index = rows.Count - 1; index >= 0; index--)
            if (!retainedIds.Contains(rows[index].Id)) rows.RemoveAt(index);
        for (int index = 0; index < desired.Count; index++)
        {
            var row = desired[index];
            if (index < rows.Count && rows[index].Id == row.Id)
            {
                if (!ReferenceEquals(rows[index], row)) rows[index] = row;
                continue;
            }
            int previous = -1;
            for (int candidate = index; candidate < rows.Count; candidate++)
                if (rows[candidate].Id == row.Id) { previous = candidate; break; }
            if (previous >= 0) rows.RemoveAt(previous);
            rows.Insert(index, row);
        }
    }

    private static void SetSidebarSearchCount(TextBlock label, int count)
    {
        label.Text = count.ToString();
        label.Visibility = count > 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private void FocusFirstSidebarSearchResult(string scope)
    {
        object? first = scope switch
        {
            "recent" => VisibleWorkRecentEntries.FirstOrDefault(), "projects" => VisibleProjectEntries.FirstOrDefault(),
            "tasks" => VisibleWorkTaskEntries.FirstOrDefault(), _ => VisibleChatEntries.FirstOrDefault()
        };
        if (first is null) return;
        var list = scope == "recent" ? WorkRecentHistory : scope == "tasks" ? WorkTaskHistory : ChatHistoryList;
        if (scope != "projects") list.ScrollIntoView(first, ScrollIntoViewAlignment.Leading);
        DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () =>
        {
            if (_chatClosing || !SidebarSearchStateFor(scope).IsOpen) return;
            Control? container = scope == "projects" ? ProjectTree.ContainerFromItem(first) as Control : list.ContainerFromItem(first) as Control;
            if (container is not null)
            {
                container.StartBringIntoView();
                container.Focus(FocusState.Keyboard);
            }
            else if (scope == "projects") ProjectTree.Focus(FocusState.Keyboard);
            else list.Focus(FocusState.Keyboard);
        });
    }

    private bool ActivateSidebarSearchResult(string scope)
    {
        if (!IsSidebarSearchScope(scope) || !_projectsReady || _chatClosing || HasPresentationModal()) return false;
        var state = SidebarSearchStateFor(scope);
        if (!state.IsOpen || state.Composing || state.SuppressEnter) return false;
        if (scope == "chat")
        {
            if (VisibleChatEntries.FirstOrDefault() is not { } row
                || !_standaloneChats.Any(chat => chat.Id == row.Id && !chat.IsArchived)) return false;
            var current = ViewModel.RecentConversations.FirstOrDefault(chat => chat.Id == row.Id);
            if (current is null) return false;
            SelectStandaloneChat(current);
            RefreshSidebarSearches("chat");
            return true;
        }
        var entry = (scope == "recent" ? VisibleWorkRecentEntries : scope == "projects" ? VisibleProjectEntries : VisibleWorkTaskEntries).FirstOrDefault();
        if (entry is null) return false;
        var project = _projects.FirstOrDefault(project => project.Id == entry.Project.Id && !project.IsArchived);
        if (project is null) return false;
        if (scope == "projects")
        {
            if (project.IsFolderlessWorkspace) return false;
            SelectWorkspaceProject(project);
            return true;
        }
        if (scope == "tasks" && _selectedWorkProjectId != project.Id) return false;
        var chat = project.Chats.FirstOrDefault(chat => chat.Id == entry.Chat?.Id && !chat.IsArchived
            && (chat.CanPersist || scope == "tasks" && chat.Id == _activeProjectChat?.Id));
        if (chat is null) return false;
        SelectProjectChat(project, chat);
        return true;
    }
}
