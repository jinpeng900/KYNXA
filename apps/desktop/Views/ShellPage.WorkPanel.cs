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
        bool isWorkConversation = !ViewModel.IsChatMode && _activeProjectChat is not null && ActiveMessages.Count > 0;
        bool canOpen = (isWorkConversation || WorkTabs.HasItems || MountedWorkspace.HasMountedFolder) && hasSpace;
        bool visible = canOpen && _layout.PreviewVisible;
        double panelWidth = visible ? GetWorkPanelWidth(available, _layout.PreviewWidth) : 0;
        double gap = visible ? WorkPanelGap : 0;

        PreviewColumn.Width = new GridLength(panelWidth);
        PreviewGripColumn.Width = new GridLength(gap);
        WorkContextPanel.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
        PresentSelectedWorkTab(visible);
        WorkPanelGrip.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
        OpenWorkPanelButton.Visibility = canOpen && !visible ? Visibility.Visible : Visibility.Collapsed;
        ConversationTitle.Margin = new Thickness(24, 16, canOpen && !visible ? 56 : 24, 0);

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
        _layout.PreviewVisible = true;
        ApplyLayout();
        SaveLayout();
    }

    private void CloseWorkPanelButton_Click(object sender, RoutedEventArgs e)
    {
        _layout.PreviewVisible = false;
        ApplyLayout();
        SaveLayout();
    }
}
