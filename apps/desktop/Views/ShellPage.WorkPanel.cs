using KYNXA_Desktop.Controls;
using Microsoft.UI.Xaml;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.Layout;
using static KYNXA_Desktop.Layout.ShellLayoutMetrics;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private double _workPanelWidthBeforeDrag;
    private Guid? _workPanelConversationId;
    private Guid? _workPanelProjectId;
    private bool _workPanelIsChatMode;
    private bool _emptyWorkPanelRequested;

    private static string? UserMountedFolder(ProjectState? project) =>
        ProjectMountPresentation.UserFolder(project, StoragePaths.DesktopDirectory);

    private void UpdateMountedWorkspacePresentation()
    {
        var project = ViewModel.IsChatMode ? null : WorkSidebarState.FindSelectedProject(_projects, _selectedWorkProjectId);
        MountedWorkspace.ShowProject(project?.Id, UserMountedFolder(project));
        ApplyLayout();
    }

    private double UpdateWorkPanelLayout()
    {
        double available = MainRegion.ActualWidth;
        bool hasSpace = CanShowWorkPanel(available, MainRegion.ActualHeight);
        SuspendUnselectedWorkViewers(_layout.PreviewVisible && hasSpace);
        SynchronizeWorkTabs();
        Guid? projectId = ViewModel.IsChatMode ? null : _selectedWorkProjectId;
        if (_workPanelConversationId != ActiveChatId || _workPanelProjectId != projectId || _workPanelIsChatMode != ViewModel.IsChatMode)
        {
            _workPanelConversationId = ActiveChatId;
            _workPanelProjectId = projectId;
            _workPanelIsChatMode = ViewModel.IsChatMode;
            _emptyWorkPanelRequested = false;
        }
        bool isWorkConversation = !ViewModel.IsChatMode && _activeProjectChat is not null && ActiveMessages.Count > 0;
        bool canOpen = (isWorkConversation || WorkTabs.HasItems || MountedWorkspace.HasMountedFolder) && hasSpace;
        // A folder header alone does not need a preview column. Empty opening belongs to the mode/project/chat scope.
        // 仅有目录标题时不占用预览列；空面板请求属于当前模式、项目与会话，未创建聊天时也不会跨项目泄漏。
        bool visible = canOpen && _layout.PreviewVisible && (WorkTabs.HasOpenTabs || _emptyWorkPanelRequested);
        double panelWidth = visible ? GetWorkPanelWidth(available, _layout.PreviewWidth) : 0;
        double gap = visible ? WorkPanelGap : 0;

        PreviewColumn.Width = new GridLength(panelWidth);
        PreviewGripColumn.Width = new GridLength(gap);
        WorkContextPanel.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
        SuspendUnselectedWorkViewers(visible);
        PresentSelectedWorkTab(visible);
        WorkPanelGrip.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
        OpenWorkPanelButton.Visibility = canOpen && !visible ? Visibility.Visible : Visibility.Collapsed;
        ConversationActionsButton.Margin = new Thickness(0, 8, canOpen && !visible ? 48 : 12, 0);
        ConversationTitle.Margin = new Thickness(24, 16, canOpen && !visible ? 88 : 52, 0);

        // Automatic hiding never changes the user's explicit open/closed preference.
        // 自动隐藏不改变用户明确选择的展开或关闭偏好。
        return Math.Max(0, available - panelWidth - gap);
    }

    private double GetWorkPanelMaximumWidth() => ShellLayoutMetrics.GetWorkPanelMaximumWidth(MainRegion.ActualWidth);

    private void WorkPanelGrip_DragStarted(object? sender, EventArgs e)
    {
        _workPanelWidthBeforeDrag = _layout.PreviewWidth;
        _dragStartValue = PreviewColumn.ActualWidth;
    }

    private void WorkPanelGrip_DragDelta(object? sender, ResizeDeltaEventArgs e)
    {
        _layout.PreviewWidth = Math.Clamp(_dragStartValue - e.Delta, WorkPanelMinimumWidth, GetWorkPanelMaximumWidth());
        ApplyLayout();
    }

    private void WorkPanelGrip_CancelRequested(object? sender, EventArgs e)
    {
        _layout.PreviewWidth = _workPanelWidthBeforeDrag;
        ApplyLayout();
    }

    private void WorkPanelGrip_ResetRequested(object? sender, EventArgs e)
    {
        _layout.PreviewWidth = 0;
        ApplyLayout();
        SaveLayout();
    }

    private void OpenWorkPanelButton_Click(object sender, RoutedEventArgs e)
    {
        _emptyWorkPanelRequested = !WorkTabs.HasOpenTabs;
        _layout.PreviewVisible = true;
        ApplyLayout();
        SaveLayout();
    }

    private void CloseWorkPanelButton_Click(object sender, RoutedEventArgs e)
    {
        _emptyWorkPanelRequested = false;
        _layout.PreviewVisible = false;
        ApplyLayout();
        SaveLayout();
    }
}
