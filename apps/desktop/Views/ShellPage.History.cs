using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.System;
using Windows.UI.Core;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private string _workHistoryQuery = string.Empty;
    private string _chatHistoryQuery = string.Empty;
    private bool _synchronizingHistorySearch;

    private void HistorySearchBox_TextChanged(object sender, TextChangedEventArgs args)
    {
        if (_synchronizingHistorySearch) return;
        if (ViewModel.IsChatMode) _chatHistoryQuery = HistorySearchBox.Text.Trim();
        else _workHistoryQuery = HistorySearchBox.Text.Trim();
        if (!_projectsReady) return;
        if (ViewModel.IsChatMode) RebuildStandaloneRows();
        else RenderProjects();
    }

    private void ResetCurrentHistorySearch()
    {
        if (ViewModel.IsChatMode) _chatHistoryQuery = string.Empty;
        else _workHistoryQuery = string.Empty;
        if (HistorySearchBox.Text.Length > 0) HistorySearchBox.Text = string.Empty;
    }

    private void SynchronizeHistorySearchMode()
    {
        _synchronizingHistorySearch = true;
        try { HistorySearchBox.Text = ViewModel.IsChatMode ? _chatHistoryQuery : _workHistoryQuery; }
        finally { _synchronizingHistorySearch = false; }
        if (_projectsReady) { RenderProjects(); RebuildStandaloneRows(); }
    }

    private void UpdateHistoryEmptyStates()
    {
        if (ChatHistoryEmpty is null || WorkHistoryEmpty is null) return;
        ChatHistoryEmpty.Text = _chatHistoryQuery.Length > 0 ? "没有匹配的聊天，试试其他关键词。" : "还没有聊天记录，从一条消息开始。";
        ChatHistoryEmpty.Visibility = ViewModel.RecentConversations.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        WorkHistoryEmpty.Text = _workHistoryQuery.Length > 0 ? "没有匹配的项目，试试其他关键词。" : "暂无关联项目，点击右上角 + 添加。";
        WorkHistoryEmpty.Visibility = ProjectEntries.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        WorkTasksPlaceholder.Text = _workHistoryQuery.Length > 0 ? "没有匹配的独立工作。" : "未关联文件夹的工作会显示在这里。";
    }

    private async void ShellPage_KeyDown(object sender, KeyRoutedEventArgs args)
    {
        if (args.Handled || _featureInfoOpen || _projectActionPending || _promptCompositionActive) return;
        // Popups and dialogs keep their own keyboard behavior.
        for (DependencyObject? child = args.OriginalSource as DependencyObject; child is not null; child = VisualTreeHelper.GetParent(child))
            if (child is ContentDialog) return;
        bool control = (InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Control) & CoreVirtualKeyStates.Down) != 0
            && (InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Shift) & CoreVirtualKeyStates.Down) == 0
            && (InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Menu) & CoreVirtualKeyStates.Down) == 0;
        if (control && args.Key == VirtualKey.F)
        {
            args.Handled = true;
            if (ShellGrid.ActualWidth < 960) _compactSidebarOpen = true;
            else if (_layout.SidebarCollapsed) { _layout.SidebarCollapsed = false; SaveLayout(); }
            ApplyLayout();
            HistorySearchBox.Focus(FocusState.Programmatic);
            HistorySearchBox.SelectAll();
        }
        else if (control && args.Key == VirtualKey.L)
        {
            args.Handled = true;
            PromptTextBox.Focus(FocusState.Programmatic);
        }
        else if (control && args.Key == VirtualKey.N && _projectsReady)
        {
            args.Handled = true;
            if (args.KeyStatus.WasKeyDown) return;
            if (ViewModel.IsChatMode) NewStandaloneChat_Click(this, new RoutedEventArgs());
            else
            {
                var project = _projects.FirstOrDefault(p => p.Chats.Contains(_activeProjectChat!));
                if (project is null && _workWithoutFolder) project = GetFolderlessWorkspace();
                if (project is null) ShowWorkspacePicker();
                else
                {
                    var target = project;
                    await RunProjectActionAsync(() => { StartWorkspaceProject(target, carryDraft: false); return Task.CompletedTask; });
                }
            }
        }
        else if (!control && args.Key == VirtualKey.Escape && _compactSidebarOpen)
        {
            args.Handled = true;
            DismissSidebar_Click(this, new RoutedEventArgs());
            CompactSidebarButton.Focus(FocusState.Programmatic);
        }
    }

    private async void ShortcutHelp_Click(object sender, RoutedEventArgs args) =>
        await ShowFeatureInfoAsync("键盘操作", "Ctrl+N  新建当前模式的对话\nCtrl+F  搜索当前模式的项目和聊天\nCtrl+L  聚焦消息输入框\nEnter  发送消息\nShift+Enter  输入换行\nEsc  收起临时侧栏\n\n中文输入法选词时，Enter 不会发送消息。");
}
