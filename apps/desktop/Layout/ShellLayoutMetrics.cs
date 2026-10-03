namespace KYNXA_Desktop.Layout;

/// <summary>Shell geometry in device-independent pixels; contains no view or persistence state.</summary>
public static class ShellLayoutMetrics
{
    public const double SidebarDefault = 240;
    public const double SidebarCollapsed = 56;
    public const double SidebarMin = 200;
    public const double SidebarMax = 360;
    public const double TopDefault = 96;
    public const double TopMin = 72;
    public const double TopMax = 180;
    public const double ComposerWidthDefault = 824;
    public const double ComposerWidthMin = 620;
    public const double ComposerWidthMax = 1040;
    public const double ComposerHeightDefault = 110;
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

    public static double GetWorkPanelMaximumWidth(double availableWidth) =>
        Math.Max(WorkPanelMinimumWidth, Math.Min(WorkPanelMaximumWidth, availableWidth - WorkChatMinimumWidth - WorkPanelGap));

    public static double GetWorkPanelWidth(double availableWidth, double requestedWidth)
    {
        double requested = double.IsFinite(requestedWidth) && requestedWidth >= WorkPanelMinimumWidth
            ? requestedWidth : Math.Clamp(availableWidth * 0.3, WorkPanelMinimumWidth, 576);
        return Math.Clamp(requested, WorkPanelMinimumWidth, GetWorkPanelMaximumWidth(availableWidth));
    }
}
