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
    public const double ConversationWidthMax = 920;
    public const double WorkPanelMinimumWidth = 320;
    public const double WorkPanelMaximumWidth = 1600;
    public const double WorkPanelOverlayMaximumWidth = 420;
    public const double WorkPanelGap = 10;
    public const double WorkChatMinimumWidth = 420;
    public const double WorkSidebarProjectHeaderHeight = 32;
    public const double WorkSidebarListRowHeight = 40;
    public const double WorkSidebarTaskChromeHeight = 60;
    public const double WorkSidebarEmptyTasksHeight = 80;

    public static double GetWorkSidebarMinimumHeight(bool hasTasks) =>
        WorkSidebarProjectHeaderHeight + WorkSidebarListRowHeight +
        (hasTasks ? WorkSidebarTaskChromeHeight + WorkSidebarListRowHeight : 0);

    /// <summary>Keep one complete row in each populated list; shorter viewports scroll the sidebar content.</summary>
    public static (double ProjectHeight, double TaskHeight, bool TasksVisible) GetWorkSidebarHeights(double availableHeight, bool hasTasks)
    {
        double height = Math.Max(GetWorkSidebarMinimumHeight(hasTasks), FiniteNonnegative(availableHeight));
        bool tasksVisible = hasTasks || height >= 180;
        double taskHeight = hasTasks ? Math.Clamp(height * .25, WorkSidebarListRowHeight,
            Math.Min(160, height - WorkSidebarProjectHeaderHeight - WorkSidebarTaskChromeHeight - WorkSidebarListRowHeight)) : 0;
        double taskChrome = tasksVisible ? hasTasks ? WorkSidebarTaskChromeHeight : WorkSidebarEmptyTasksHeight : 0;
        double projectHeight = Math.Min(320, Math.Min(height * .65,
            height - WorkSidebarProjectHeaderHeight - taskChrome - taskHeight));
        return (projectHeight, taskHeight, tasksVisible);
    }

    public static (double Minimum, double Maximum) GetComposerWidthRange(double availableWidth)
    {
        availableWidth = FiniteNonnegative(availableWidth);
        double padding = availableWidth switch { >= 1200 => 144, >= 800 => 96, >= 520 => 56, _ => 24 };
        // Preserve normal padding, but leave room for the editor even in a very narrow viewport.
        double maximum = Math.Min(ComposerWidthMax, availableWidth - Math.Min(padding, availableWidth / 2));
        return (Math.Min(ComposerWidthMin, maximum), maximum);
    }

    public static double GetComposerHeightMaximum(double availableHeight, double footerHeight)
    {
        // Zero is the initial, unmeasured layout pass; keep the existing default until measured.
        if (!double.IsFinite(availableHeight) || availableHeight <= 0) return ComposerHeightMax;
        double remaining = Math.Max(0, availableHeight - 32 - FiniteNonnegative(footerHeight));
        return Math.Min(ComposerHeightMax, remaining);
    }

    /// <summary>Whether the panel can sit beside the chat; smaller windows use an overlay.</summary>
    public static bool CanShowWorkPanel(double availableWidth, double availableHeight) =>
        double.IsFinite(availableWidth) && double.IsFinite(availableHeight)
        && availableWidth >= 900 && availableHeight >= 440;

    /// <summary>Fit an overlay to the main region, including regions narrower than the usual panel minimum.</summary>
    public static double GetWorkPanelOverlayWidth(double availableWidth) =>
        Math.Min(WorkPanelOverlayMaximumWidth, FiniteNonnegative(availableWidth));

    public static double GetWorkPanelMaximumWidth(double availableWidth) =>
        Math.Max(WorkPanelMinimumWidth, Math.Min(WorkPanelMaximumWidth,
            FiniteNonnegative(availableWidth) - WorkChatMinimumWidth - WorkPanelGap));

    public static double GetWorkPanelWidth(double availableWidth, double requestedWidth)
    {
        double requested = double.IsFinite(requestedWidth) && requestedWidth >= WorkPanelMinimumWidth
            ? requestedWidth : Math.Clamp(FiniteNonnegative(availableWidth) * 0.3, WorkPanelMinimumWidth, 576);
        return Math.Clamp(requested, WorkPanelMinimumWidth, GetWorkPanelMaximumWidth(availableWidth));
    }

    private static double FiniteNonnegative(double value) => double.IsFinite(value) && value > 0 ? value : 0;
}
