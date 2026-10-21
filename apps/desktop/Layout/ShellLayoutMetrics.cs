namespace KYNXA_Desktop.Layout;

/// <summary>
/// Shell geometry in device-independent pixels; contains no view or persistence state.
/// 主界面几何尺寸使用设备无关像素；不包含视图或持久化状态。
/// </summary>
public static class ShellLayoutMetrics
{
    public const double SidebarDefault = 240;
    public const double CompactSidebarBreakpoint = 980;
    public const double SidebarCollapsed = 48;
    public const double SidebarGripGutter = 4;
    public const double SidebarMin = 200;
    public const double SidebarMax = 360;
    public const double TopDefault = 96;
    public const double TopMin = 72;
    public const double TopMax = 180;
    public const double ComposerWidthDefault = 824;
    public const double ComposerWidthMin = 620;
    public const double ComposerWidthMax = 1040;
    public const double ComposerHeightDefault = 104;
    public const double ComposerHeightMin = 96;
    public const double ComposerAutoHeightMax = 210;
    public const double ComposerHeightMax = 420;
    public const double WorkPanelMinimumWidth = 320;
    public const double WorkPanelMaximumWidth = 1600;
    public const double WorkPanelGap = 10;
    public const double WorkChatMinimumWidth = 420;

    public static (double Minimum, double Maximum) GetWorkNavigationHeightRange(double availableHeight, bool bothExpanded = false)
    {
        // Keep the outer grip and tasks usable before allocating navigation space.
        // 分配导航区空间前，先保证外部分隔线和任务区可用。
        double maximum = double.IsFinite(availableHeight) ? Math.Max(0, availableHeight - 8 - 96) : 0;
        double minimum = Math.Min(bothExpanded ? 208 : 136, maximum);
        return (minimum, maximum);
    }

    public static (double Minimum, double Maximum) GetWorkRecentHeightRange(double contentBudget)
    {
        double budget = double.IsFinite(contentBudget) ? Math.Max(0, contentBudget) : 0;
        double minimum = Math.Min(64, budget * 0.5);
        return (minimum, Math.Max(minimum, budget - minimum));
    }

    public static (double Minimum, double Maximum) GetComposerWidthRange(double availableWidth)
    {
        double padding = availableWidth switch { >= 1200 => 144, >= 800 => 96, >= 520 => 56, _ => 24 };
        double maximum = Math.Min(ComposerWidthMax, Math.Max(240, availableWidth - padding));
        return (Math.Min(ComposerWidthMin, maximum), maximum);
    }

    public static double GetComposerHeightMaximum(double availableHeight, double footerHeight) =>
        availableHeight <= 0 ? ComposerHeightMax : Math.Min(ComposerHeightMax, Math.Max(72, availableHeight - 32 - footerHeight));

    public static bool CanShowWorkPanel(double availableWidth, double availableHeight) =>
        availableWidth >= 900 && availableHeight >= 440;

    /// <summary>
    /// A compact viewport uses the existing workspace as an overlay rather than removing its entry point.
    /// 紧凑视口复用当前工作区作为覆盖栏，保留入口，不要求聊天与右栏同时容纳在两列内。
    /// </summary>
    public static bool CanOpenWorkPanel(double availableWidth, double availableHeight) =>
        double.IsFinite(availableWidth) && double.IsFinite(availableHeight) && availableWidth > 0 && availableHeight > 0;

    public static double GetWorkPanelOverlayWidth(double availableWidth, double requestedWidth)
    {
        if (!double.IsFinite(availableWidth) || availableWidth <= 0) return 0;
        double maximum = Math.Min(WorkPanelMaximumWidth, availableWidth);
        double minimum = Math.Min(WorkPanelMinimumWidth, maximum);
        double requested = double.IsFinite(requestedWidth) && requestedWidth >= WorkPanelMinimumWidth
            ? requestedWidth : Math.Clamp(availableWidth * 0.8, minimum, Math.Min(576, maximum));
        return Math.Clamp(requested, minimum, maximum);
    }

    public static double GetWorkPanelMaximumWidth(double availableWidth) =>
        Math.Max(WorkPanelMinimumWidth, Math.Min(WorkPanelMaximumWidth, availableWidth - WorkChatMinimumWidth - WorkPanelGap));

    public static double GetWorkPanelWidth(double availableWidth, double requestedWidth)
    {
        double requested = double.IsFinite(requestedWidth) && requestedWidth >= WorkPanelMinimumWidth
            ? requestedWidth : Math.Clamp(availableWidth * 0.3, WorkPanelMinimumWidth, 576);
        return Math.Clamp(requested, WorkPanelMinimumWidth, GetWorkPanelMaximumWidth(availableWidth));
    }
}
