namespace KYNXA_Desktop.Layout;

public sealed record WorkSidebarLayoutResult(double RecentHeight, double ProjectsHeight, double TasksHeight);

/// <summary>
/// Bounds each sidebar list by its visible rows and shares only genuinely constrained space.
/// 按可见条数限制各侧栏列表，只在窗口高度不足时分配受限空间。
/// </summary>
public static class WorkSidebarLayout
{
    public static WorkSidebarLayoutResult? Calculate(double availableHeight, bool recentExpanded,
        bool projectsExpanded, bool tasksExpanded, int recentCount, int projectVisibleCount, int taskCount,
        double rowHeight = 40)
    {
        // A detached view retains its last geometry; old divider ratios no longer drive layout.
        // 脱离视觉树的视图保留上次几何尺寸；旧分隔线比例不再参与布局。
        if (!double.IsFinite(availableHeight) || availableHeight <= 0) return null;
        if (!double.IsFinite(rowHeight) || rowHeight <= 0) rowHeight = 40;
        double Desired(int count, bool expanded) => expanded
            ? Math.Clamp(count, 0, ShellLayoutMetrics.WorkSidebarVisibleRows) * rowHeight : 0;
        double[] desired = [Desired(recentCount, recentExpanded), Desired(projectVisibleCount, projectsExpanded),
            Desired(taskCount, tasksExpanded)];
        double budget = Math.Max(0, availableHeight - ShellLayoutMetrics.WorkSidebarHeaderHeight);
        if (desired.Sum() <= budget) return new(desired[0], desired[1], desired[2]);

        // Satisfy short lists first, then share the remainder across longer scrollable lists.
        // 优先完整保留短列表，再让较长的可滚动列表均分剩余空间。
        double[] allocated = new double[desired.Length];
        var pending = Enumerable.Range(0, desired.Length).Where(index => desired[index] > 0).ToList();
        while (pending.Count > 0)
        {
            double share = budget / pending.Count;
            var fitting = pending.Where(index => desired[index] <= share).ToArray();
            if (fitting.Length == 0)
            {
                foreach (int index in pending) allocated[index] = share;
                break;
            }
            foreach (int index in fitting)
            {
                allocated[index] = desired[index];
                budget -= desired[index];
                pending.Remove(index);
            }
        }
        return new(allocated[0], allocated[1], allocated[2]);
    }
}
