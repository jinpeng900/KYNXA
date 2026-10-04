using KYNXA_Desktop.Models.UI;

namespace KYNXA_Desktop.Layout;

public enum WorkSidebarRowSizing
{
    Auto,
    Pixels,
    Star
}

public readonly record struct WorkSidebarRowHeight(WorkSidebarRowSizing Sizing, double Value = 0);

public sealed record WorkSidebarLayoutResult(
    WorkSidebarRowHeight NavigationRow,
    WorkSidebarRowHeight RecentRow,
    WorkSidebarRowHeight ProjectsRow,
    double NavigationGripHeight,
    double RecentGripHeight,
    double RecentMaximumHeight,
    double ProjectsMaximumHeight);

/// <summary>
/// Allocates work sidebar space and owns divider drag snapshots without view or storage dependencies.
/// 计算工作侧栏空间并保存分隔线拖动快照，不依赖视图或存储。
/// </summary>
public sealed class WorkSidebarLayout
{
    private const double NavigationHeaderHeight = 72;
    private const double DividerHeight = 8;
    private const double MinimumDragDelta = 0.5;
    private NavigationDrag _navigationDrag;
    private RecentDrag _recentDrag;

    private readonly record struct NavigationDrag(double Height, double Ratio);
    private readonly record struct RecentDrag(double Height, double NavigationHeight, double Ratio, double NavigationRatio);

    public static WorkSidebarLayoutResult? Calculate(double availableHeight, bool recentExpanded, bool projectsExpanded,
        double navigationRatio, double recentRatio)
    {
        // An unmeasured or detached view must retain its last applied geometry.
        // 未测量或已脱离视觉树的视图保留上次应用的几何尺寸。
        if (!double.IsFinite(availableHeight) || availableHeight <= 0) return null;
        bool bothExpanded = recentExpanded && projectsExpanded;
        bool anyExpanded = recentExpanded || projectsExpanded;
        var automatic = new WorkSidebarRowHeight(WorkSidebarRowSizing.Auto);
        var remaining = new WorkSidebarRowHeight(WorkSidebarRowSizing.Star, 1);
        var projectsRow = projectsExpanded ? remaining : automatic;
        if (!anyExpanded)
            return new(automatic, automatic, projectsRow, 0, 0, 0, 0);

        var (minimum, maximum) = ShellLayoutMetrics.GetWorkNavigationHeightRange(availableHeight, bothExpanded);
        bool resized = double.IsFinite(navigationRatio) && navigationRatio > 0;
        double navigationHeight = resized
            // A naturally short boundary saved by the inner grip must not jump down.
            // 内部分隔线保存的较短自然边界不能突然向下跳动。
            ? Math.Clamp(availableHeight * navigationRatio, Math.Min(minimum, NavigationHeaderHeight + (bothExpanded ? DividerHeight : 0)), maximum)
            : Math.Clamp(availableHeight * 0.5, minimum, maximum);
        var navigationRow = resized ? new WorkSidebarRowHeight(WorkSidebarRowSizing.Pixels, navigationHeight) : automatic;
        double navigationBudget = Math.Max(0, navigationHeight - NavigationHeaderHeight - (bothExpanded ? DividerHeight : 0));
        var (recentMinimum, recentMaximum) = ShellLayoutMetrics.GetWorkRecentHeightRange(navigationBudget);
        bool recentResized = bothExpanded && double.IsFinite(recentRatio) && recentRatio > 0;
        double recentHeight = bothExpanded
            ? Math.Clamp(navigationBudget * (recentResized ? recentRatio : 0.5), recentMinimum, recentMaximum)
            : navigationBudget;
        WorkSidebarRowHeight recentRow;
        if (recentResized) recentRow = new(WorkSidebarRowSizing.Pixels, recentHeight);
        else if (recentExpanded && !projectsExpanded && resized) recentRow = remaining;
        else recentRow = automatic;
        double projectsMaximum;
        if (resized) projectsMaximum = double.PositiveInfinity;
        else if (bothExpanded) projectsMaximum = navigationBudget - recentHeight;
        else projectsMaximum = navigationBudget;
        return new(navigationRow, recentRow, projectsRow, DividerHeight, bothExpanded ? DividerHeight : 0,
            recentHeight, projectsMaximum);
    }

    public void BeginNavigationDrag(double actualHeight, LayoutState preferences) =>
        _navigationDrag = new(actualHeight, preferences.WorkNavigationRatio);

    public bool ResizeNavigation(LayoutState preferences, double availableHeight, bool recentExpanded, bool projectsExpanded,
        double delta)
    {
        if (availableHeight <= 0 || Math.Abs(delta) < MinimumDragDelta) return false;
        var (minimum, maximum) = ShellLayoutMetrics.GetWorkNavigationHeightRange(availableHeight,
            recentExpanded && projectsExpanded);
        preferences.WorkNavigationRatio = Math.Clamp(_navigationDrag.Height + delta, minimum, maximum) / availableHeight;
        return true;
    }

    public void CancelNavigationDrag(LayoutState preferences) => preferences.WorkNavigationRatio = _navigationDrag.Ratio;

    public void ResetNavigation(LayoutState preferences) => preferences.WorkNavigationRatio = 0;

    public void BeginRecentDrag(double actualHeight, double navigationHeight, LayoutState preferences) =>
        _recentDrag = new(actualHeight, navigationHeight, preferences.WorkRecentRatio, preferences.WorkNavigationRatio);

    public bool ResizeRecent(LayoutState preferences, double availableHeight, double delta)
    {
        double budget = Math.Max(0, _recentDrag.NavigationHeight - NavigationHeaderHeight - DividerHeight);
        if (budget <= 0 || availableHeight <= 0 || Math.Abs(delta) < MinimumDragDelta) return false;
        // Freeze an automatic outer boundary only when dragged; keep an existing preference intact.
        // 只有拖动时才固定自动外部边界；已有用户偏好保持不变。
        if (!double.IsFinite(_recentDrag.NavigationRatio) || _recentDrag.NavigationRatio <= 0)
            preferences.WorkNavigationRatio = _recentDrag.NavigationHeight / availableHeight;
        var (minimum, maximum) = ShellLayoutMetrics.GetWorkRecentHeightRange(budget);
        preferences.WorkRecentRatio = Math.Clamp(_recentDrag.Height + delta, minimum, maximum) / budget;
        return true;
    }

    public void CancelRecentDrag(LayoutState preferences)
    {
        preferences.WorkRecentRatio = _recentDrag.Ratio;
        preferences.WorkNavigationRatio = _recentDrag.NavigationRatio;
    }

    public void ResetRecent(LayoutState preferences) => preferences.WorkRecentRatio = 0;
}
