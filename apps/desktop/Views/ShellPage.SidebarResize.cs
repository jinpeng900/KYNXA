using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Layout;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private sealed record SidebarResize(int Boundary, double Budget, WorkSidebarLayoutResult Heights,
        WorkSidebarLayoutResult Maximum, double RecentRatio, double ProjectsRatio, double TasksRatio);
    private SidebarResize? _sidebarResize;
    private double _workSidebarListBudget;

    private void WorkSidebarDivider_DragStarted(object? sender, EventArgs e)
    {
        if (sender is not FrameworkElement { Tag: string tag } || !int.TryParse(tag, out int boundary) ||
            _workSidebarListBudget <= 0) return;
        double row = (double)Application.Current.Resources["KynxaSidebarRowHeight"];
        var maximum = new WorkSidebarLayoutResult(_layout.WorkRecentExpanded ? VisibleWorkRecentEntries.Count * row : 0,
            ProjectTree.Visibility == Visibility.Visible ? VisibleProjectRows(VisibleProjectEntries) * row : 0,
            _layout.WorkTasksExpanded ? VisibleWorkTaskEntries.Count * row : 0);
        _sidebarResize = new(boundary, _workSidebarListBudget,
            new(WorkRecentHistory.Height, ProjectTree.Height, WorkTaskHistory.Height), maximum,
            _layout.WorkRecentHeightRatio, _layout.WorkProjectsHeightRatio, _layout.WorkTasksHeightRatio);
    }

    private void WorkSidebarDivider_DragDelta(object? sender, ResizeDeltaEventArgs e)
    {
        if (_sidebarResize is not { } drag || !double.IsFinite(e.Delta)) return;
        double row = (double)Application.Current.Resources["KynxaSidebarRowHeight"];
        var next = WorkSidebarLayout.Resize(drag.Heights, drag.Boundary, e.Delta, drag.Budget, drag.Maximum, row);
        // Keep hidden sections' saved sizes; pointer movement only changes layout preferences.
        // 隐藏分区保留原高度；拖动只更新布局偏好，不重建会话列表。
        if (next == drag.Heights)
        {
            _layout.WorkRecentHeightRatio = drag.RecentRatio;
            _layout.WorkProjectsHeightRatio = drag.ProjectsRatio;
            _layout.WorkTasksHeightRatio = drag.TasksRatio;
        }
        else
        {
            if (drag.Maximum.RecentHeight > 0) _layout.WorkRecentHeightRatio = next.RecentHeight / drag.Budget;
            if (drag.Maximum.ProjectsHeight > 0) _layout.WorkProjectsHeightRatio = next.ProjectsHeight / drag.Budget;
            if (drag.Maximum.TasksHeight > 0) _layout.WorkTasksHeightRatio = next.TasksHeight / drag.Budget;
        }
        UpdateWorkSidebarHeights(WorkSidebarContent.ActualHeight);
    }

    private void WorkSidebarDivider_DragCompleted(object? sender, EventArgs e)
    {
        if (_sidebarResize is null) return;
        _sidebarResize = null;
        SaveLayout();
    }

    private void WorkSidebarDivider_CancelRequested(object? sender, EventArgs e)
    {
        if (_sidebarResize is not { } drag) return;
        _sidebarResize = null;
        _layout.WorkRecentHeightRatio = drag.RecentRatio;
        _layout.WorkProjectsHeightRatio = drag.ProjectsRatio;
        _layout.WorkTasksHeightRatio = drag.TasksRatio;
        UpdateWorkSidebarHeights(WorkSidebarContent.ActualHeight);
    }

    private void WorkSidebarDivider_ResetRequested(object? sender, EventArgs e)
    {
        _sidebarResize = null;
        _layout.WorkRecentHeightRatio = _layout.WorkProjectsHeightRatio = _layout.WorkTasksHeightRatio = 0;
        UpdateWorkSidebarHeights(WorkSidebarContent.ActualHeight);
        SaveLayout();
    }
}
