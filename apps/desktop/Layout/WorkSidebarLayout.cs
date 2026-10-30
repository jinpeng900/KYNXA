namespace KYNXA_Desktop.Layout;

public sealed record WorkSidebarLayoutResult(double RecentHeight, double ProjectsHeight, double TasksHeight);

/// <summary>
/// Starts with four rows per list, then fits remembered or dragged heights to the viewport.
/// 列表默认四行，拖动及保存的高度按当前视口分配。
/// </summary>
public static class WorkSidebarLayout
{
    public static WorkSidebarLayoutResult? Calculate(double availableHeight, bool recentExpanded,
        bool projectsExpanded, bool tasksExpanded, int recentCount, int projectVisibleCount, int taskCount,
        double rowHeight = 40, double recentRatio = 0, double projectsRatio = 0, double tasksRatio = 0)
    {
        // A detached view retains its last geometry.
        // 脱离视觉树的视图保留上次几何尺寸。
        if (!double.IsFinite(availableHeight) || availableHeight <= 0) return null;
        if (!double.IsFinite(rowHeight) || rowHeight <= 0) rowHeight = 40;
        double budget = Math.Max(0, availableHeight - ShellLayoutMetrics.WorkSidebarHeaderHeight);
        double Desired(int count, bool expanded, double ratio)
        {
            if (!expanded || count <= 0) return 0;
            double maximum = count * rowHeight;
            double desired = double.IsFinite(ratio) && ratio > 0 ? Math.Min(ratio, 1) * budget
                : Math.Min(count, ShellLayoutMetrics.WorkSidebarVisibleRows) * rowHeight;
            return Math.Clamp(desired, Math.Min(rowHeight, maximum), maximum);
        }
        double[] desired = [Desired(recentCount, recentExpanded, recentRatio),
            Desired(projectVisibleCount, projectsExpanded, projectsRatio), Desired(taskCount, tasksExpanded, tasksRatio)];
        return Result(Fit(desired, budget, rowHeight));
    }

    public static WorkSidebarLayoutResult Resize(WorkSidebarLayoutResult current, int boundary, double delta,
        double contentBudget, WorkSidebarLayoutResult maximum, double rowHeight = 40)
    {
        if (boundary is not (0 or 1)) throw new ArgumentOutOfRangeException(nameof(boundary));
        if (!double.IsFinite(rowHeight) || rowHeight <= 0) rowHeight = 40;
        double budget = double.IsFinite(contentBudget) ? Math.Max(0, contentBudget) : 0;
        double[] limits = [maximum.RecentHeight, maximum.ProjectsHeight, maximum.TasksHeight];
        double[] heights = [current.RecentHeight, current.ProjectsHeight, current.TasksHeight];
        for (int index = 0; index < heights.Length; index++)
        {
            limits[index] = double.IsFinite(limits[index]) ? Math.Clamp(limits[index], 0, budget) : 0;
            double height = double.IsFinite(heights[index]) ? heights[index] : 0;
            heights[index] = Math.Clamp(height, Math.Min(rowHeight, limits[index]), limits[index]);
        }
        heights = Fit(heights, budget, rowHeight);
        if (!double.IsFinite(delta)) return Result(heights);
        int upper = boundary, lower = boundary + 1;
        while (lower < limits.Length && limits[lower] == 0) lower++;
        bool hasLower = lower < limits.Length;
        double upperMinimum = Math.Min(rowHeight, limits[upper]);
        double lowerMinimum = hasLower ? Math.Min(rowHeight, limits[lower]) : 0;
        if (delta > 0)
        {
            // Use free space before shortening the next list.
            // 先使用底部空白，再压缩相邻列表。
            double spare = Math.Max(0, budget - heights.Sum());
            double growth = Math.Min(delta, Math.Max(0, limits[upper] - heights[upper]));
            growth = Math.Min(growth, spare + (hasLower ? Math.Max(0, heights[lower] - lowerMinimum) : 0));
            heights[upper] += growth;
            if (hasLower) heights[lower] -= Math.Max(0, growth - spare);
        }
        else
        {
            double shrink = Math.Min(-delta, Math.Max(0, heights[upper] - upperMinimum));
            heights[upper] -= shrink;
            if (hasLower) heights[lower] += Math.Min(shrink, Math.Max(0, limits[lower] - heights[lower]));
        }
        return Result(heights);
    }

    private static double[] Fit(double[] desired, double budget, double rowHeight)
    {
        if (desired.Sum() <= budget) return desired;
        double[] allocated = desired.Select(height => Math.Min(height, rowHeight)).ToArray();
        double minimum = allocated.Sum();
        if (minimum > budget)
            return allocated.Select(height => minimum > 0 ? height * budget / minimum : 0).ToArray();
        budget -= minimum;
        // Keep one row per active list, then satisfy short lists before sharing the remainder.
        // 各展开列表先保留一行，再满足短列表并分配剩余空间。
        var pending = Enumerable.Range(0, desired.Length).Where(index => desired[index] > allocated[index]).ToList();
        while (pending.Count > 0)
        {
            double share = budget / pending.Count;
            var fitting = pending.Where(index => desired[index] - allocated[index] <= share).ToArray();
            if (fitting.Length == 0)
            {
                foreach (int index in pending) allocated[index] += share;
                break;
            }
            foreach (int index in fitting)
            {
                budget = Math.Max(0, budget - (desired[index] - allocated[index]));
                allocated[index] = desired[index];
                pending.Remove(index);
            }
        }
        return allocated;
    }

    private static WorkSidebarLayoutResult Result(double[] heights) => new(heights[0], heights[1], heights[2]);
}
