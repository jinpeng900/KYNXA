using System.Text.Json;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private double _taskRatioBeforeDrag, _taskHeightBeforeDrag;

    private void UpdateTaskRegionLayout()
    {
        if (TaskPanel is null || WorkContextModulesHost is null) return;
        double height = WorkContextModulesHost.ActualHeight;
        double ratio = _layout.WorkTaskRatio > 0 ? Math.Clamp(_layout.WorkTaskRatio, 0.12, 0.35) : 0.25;
        TaskPanel.SetAvailableHeight(Math.Max(72, height * ratio));
        // The native webpage owns only the remaining preview row; shell task and file rows keep their space.
        // 原生网页只使用剩余预览行，任务标题和文件标签始终保留各自的布局空间。
        WorkTaskRow.MinHeight = ActiveChatId is not null ? 34 : 0;
        WorkFileTabsRow.MinHeight = 42;
        TaskPanel.Visibility = ActiveChatId is not null ? Visibility.Visible : Visibility.Collapsed;
        TaskRegionGrip.Visibility = ActiveChatId is not null && TaskPanel.IsExpanded ? Visibility.Visible : Visibility.Collapsed;
    }

    private void WorkContextModulesHost_SizeChanged(object sender, SizeChangedEventArgs args) => UpdateTaskRegionLayout();
    private void TaskRegionGrip_DragStarted(object? sender, EventArgs args)
    { _taskRatioBeforeDrag = _layout.WorkTaskRatio; _taskHeightBeforeDrag = TaskPanel.ActualHeight; }
    private void TaskRegionGrip_DragDelta(object? sender, ResizeDeltaEventArgs args)
    {
        if (WorkContextModulesHost.ActualHeight <= 0) return;
        _layout.WorkTaskRatio = Math.Clamp((_taskHeightBeforeDrag + args.Delta) / WorkContextModulesHost.ActualHeight, 0.12, 0.35);
        UpdateTaskRegionLayout();
    }
    private void TaskRegionGrip_CancelRequested(object? sender, EventArgs args)
    { _layout.WorkTaskRatio = _taskRatioBeforeDrag; UpdateTaskRegionLayout(); }
    private void TaskRegionGrip_ResetRequested(object? sender, EventArgs args)
    { _layout.WorkTaskRatio = 0; UpdateTaskRegionLayout(); SaveLayout(); }

    private async void OpenWorkspaceFile() => await BrowseWorkspaceFilesAsync();

    private string? CurrentWorkspaceDirectory() => CurrentWorkspaceMount()?.Path ?? CurrentWorkspaceProject()?.FolderPath;

    private void OpenWorkspaceChanges()
    {
        if (_chatClosing || ActiveChatId is not Guid conversation) return;
        var mount = CurrentWorkspaceMount();
        string title = UiText.Get("未提交变更") + (mount is null ? "" : " · " + mount.Name);
        AddWorkspaceTab(conversation, new($"changes/{conversation:D}/{mount?.Id:D}", "changes", title,
            conversation.ToString("D"), MountId: mount?.Id), select: true);
    }

    private void OpenWorkspaceTerminal()
    {
        if (_chatClosing || ActiveChatId is not Guid conversation) return;
        int number = ResourcesFor(conversation).Items.Values.Count(item => item.Tab.Kind == "terminal") + 1;
        var mount = CurrentWorkspaceMount();
        string title = $"{UiText.Get("终端")} {number}" + (mount is null ? "" : $" · {mount.Name}");
        // A tab owns one PTY; opening another terminal must not select or replace the conversation's first session.
        // 每个标签拥有独立伪终端；再次新建不能选回或替换聊天中的第一个会话。
        AddWorkspaceTab(conversation, new($"terminal/{conversation:D}/{Guid.NewGuid():N}", "terminal", title,
            conversation.ToString("D"), MountId: mount?.Id), select: true);
    }

    private void AddWebsiteTab(string url, Guid? conversationId = null)
    {
        if ((conversationId ?? ActiveChatId) is not Guid conversation ||
            !Uri.TryCreate(url, UriKind.Absolute, out var address) || address.Scheme is not ("http" or "https")) return;
        var reference = JsonSerializer.SerializeToElement(new { url = address.AbsoluteUri });
        if (ConversationWorkspaceSources.FromReference(reference) is { } tab) AddWorkspaceTab(conversation, tab, select: true);
    }
}
