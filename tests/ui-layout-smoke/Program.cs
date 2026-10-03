using KYNXA_Desktop.Layout;
using KYNXA_Desktop.Models.UI;

static void Check(bool condition, string message)
{
    if (!condition) throw new InvalidOperationException(message);
}

// At the 360-DIP window minimum, the title bar and sidebar controls can leave only 61 DIP.
// The outer sidebar scroller must retain complete project/task rows inside that viewport.
foreach (bool hasTasks in new[] { false, true })
{
    foreach (double available in new[] { 1d, 32, 61, 72, 100, 120, 136, 172, 179, 180, 260, 400, 900, double.MaxValue,
        double.NaN, double.PositiveInfinity, double.NegativeInfinity, 0, -1 })
    {
        var heights = ShellLayoutMetrics.GetWorkSidebarHeights(available, hasTasks);
        double minimumHeight = ShellLayoutMetrics.GetWorkSidebarMinimumHeight(hasTasks);
        double contentHeight = double.IsFinite(available) ? Math.Max(minimumHeight, available) : minimumHeight;
        Check(double.IsFinite(heights.ProjectHeight) && double.IsFinite(heights.TaskHeight),
            "Sidebar measurements must not send a non-finite height to XAML.");
        Check(heights.ProjectHeight >= ShellLayoutMetrics.WorkSidebarListRowHeight && heights.ProjectHeight <= 320,
            "The work sidebar must retain at least one complete project row, including short viewports.");
        Check(heights.TaskHeight >= 0 && heights.TaskHeight <= 160,
            "The task list must remain inside its supported height range.");
        if (hasTasks)
            Check(heights.TasksVisible && heights.TaskHeight >= ShellLayoutMetrics.WorkSidebarListRowHeight,
                "A populated task list must retain one complete row in the scrollable sidebar.");
        else
            Check(heights.TaskHeight == 0, "An empty task list must not consume a list viewport.");
        double taskChrome = heights.TasksVisible
            ? hasTasks ? ShellLayoutMetrics.WorkSidebarTaskChromeHeight : ShellLayoutMetrics.WorkSidebarEmptyTasksHeight : 0;
        Check(ShellLayoutMetrics.WorkSidebarProjectHeaderHeight + heights.ProjectHeight + taskChrome + heights.TaskHeight <= contentHeight,
            "Project title, task chrome and list viewports must fit inside the scrollable sidebar content.");
        if (!hasTasks && contentHeight < 180)
            Check(!heights.TasksVisible, "Hidden empty tasks must not reserve space in a short sidebar.");
    }
}
Check(ShellLayoutMetrics.GetWorkSidebarHeights(120, false).ProjectHeight > 0,
    "Regression: hiding empty tasks must not leave a 136-DIP reservation that collapses the project tree.");

foreach (double unavailable in new[] { double.NaN, double.PositiveInfinity, double.NegativeInfinity, 0, -1 })
{
    var (min, max) = ShellLayoutMetrics.GetComposerWidthRange(unavailable);
    Check(double.IsFinite(min) && double.IsFinite(max), "Unmeasured composer width must not pass a non-finite value to XAML.");
    Check(!ShellLayoutMetrics.CanShowWorkPanel(unavailable, 600), "Unmeasured width must not select a side-by-side panel.");
    Check(!ShellLayoutMetrics.CanShowWorkPanel(1200, unavailable), "Unmeasured height must not select a side-by-side panel.");
    double panelWidth = ShellLayoutMetrics.GetWorkPanelWidth(unavailable, double.NaN);
    Check(double.IsFinite(panelWidth), "Unmeasured panel width must not pass a non-finite value to XAML.");
    Check(ShellLayoutMetrics.GetWorkPanelOverlayWidth(unavailable) == 0, "An overlay must wait for a measured viewport width.");
}

foreach (double available in new[] { 1d, 24, 64, 120, 239, 240, 260, 319, 420, 600, 899, 900, 3200, double.MaxValue })
{
    var (min, max) = ShellLayoutMetrics.GetComposerWidthRange(available);
    Check(double.IsFinite(min) && double.IsFinite(max) && min >= 0 && min <= max && max <= available,
        "A compact composer must fit the viewport without a non-finite or negative width.");
    double overlay = ShellLayoutMetrics.GetWorkPanelOverlayWidth(available);
    Check(double.IsFinite(overlay) && overlay > 0 && overlay <= available,
        "A narrow window must retain an accessible overlay inside its viewport.");
    Check(overlay <= ShellLayoutMetrics.WorkPanelOverlayMaximumWidth, "The overlay must remain compact on a wide window.");
    if (available >= ShellLayoutMetrics.WorkPanelMinimumWidth)
        Check(overlay >= ShellLayoutMetrics.WorkPanelMinimumWidth, "An overlay must retain a readable width when space permits.");
}

for (double available = 320; available <= 3200; available += 10)
{
    var (min, max) = ShellLayoutMetrics.GetComposerWidthRange(available);
    Check(min > 0 && min <= max && max < available, "Composer must fit the conversation region with padding.");
    Check(max <= ShellLayoutMetrics.ComposerWidthMax, "Composer exceeded its maximum width.");
    if (!ShellLayoutMetrics.CanShowWorkPanel(available, 600)) continue;
    foreach (double requested in new[] { 0, -100, 320, 500, 1600, 5000, double.NaN, double.PositiveInfinity })
    {
        double width = ShellLayoutMetrics.GetWorkPanelWidth(available, requested);
        Check(double.IsFinite(width) && width >= 320 && width <= 1600, "Right panel width escaped its valid range.");
        Check(available - width - ShellLayoutMetrics.WorkPanelGap >= 420, "Right panel squeezed the chat below its minimum.");
    }
}
Check(!ShellLayoutMetrics.CanShowWorkPanel(899, 900), "A narrow window should use an overlay instead of side-by-side columns.");
Check(!ShellLayoutMetrics.CanShowWorkPanel(1400, 439), "A short window should use a scrollable overlay instead of side-by-side columns.");
Check(ShellLayoutMetrics.CanShowWorkPanel(900, 440), "Right panel should fit at the minimum supported size.");
Check(ShellLayoutMetrics.GetWorkPanelOverlayWidth(899) > 0 && ShellLayoutMetrics.GetWorkPanelOverlayWidth(1400) > 0,
    "Switching to overlay layout must not remove access to the right panel.");
double preferred = 1400;
double narrow = ShellLayoutMetrics.GetWorkPanelWidth(1000, preferred);
double restored = ShellLayoutMetrics.GetWorkPanelWidth(2200, preferred);
Check(narrow < preferred && restored == preferred, "Temporary window constraints must preserve the user's preferred width.");
foreach (double available in new[] { 260d, 400, 700 })
{
    double editor = ShellLayoutMetrics.GetComposerHeightMaximum(available, 44);
    Check(editor + 44 + 32 <= available, "Connected footer must be included in the height budget.");
}
foreach (double available in new[] { 1d, 32, 64, 76, 100, 120, 147, 148, 260, 400, 700 })
{
    foreach (double footer in new[] { 0d, 44, 100 })
    {
        double editor = ShellLayoutMetrics.GetComposerHeightMaximum(available, footer);
        Check(double.IsFinite(editor) && editor >= 0 && editor <= ShellLayoutMetrics.ComposerHeightMax,
            "A short viewport must produce a valid editor height.");
        if (available >= footer + 32)
            Check(editor + footer + 32 <= available, "A short editor must honor the measured footer and padding budget.");
        else
            Check(editor == 0, "A viewport consumed by footer and padding must not add an overflowing editor.");
    }
}
foreach (double unavailable in new[] { double.NaN, double.PositiveInfinity, double.NegativeInfinity, 0, -1 })
{
    double editor = ShellLayoutMetrics.GetComposerHeightMaximum(unavailable, 44);
    Check(double.IsFinite(editor) && editor == ShellLayoutMetrics.ComposerHeightMax,
        "Unmeasured height must keep the initial editor-height fallback valid.");
    double invalidFooterEditor = ShellLayoutMetrics.GetComposerHeightMaximum(300, unavailable);
    Check(double.IsFinite(invalidFooterEditor) && invalidFooterEditor >= 0 && invalidFooterEditor + 32 <= 300,
        "An invalid footer measurement must not corrupt editor sizing.");
}
var defaults = LayoutState.CreateDefault();
Check(defaults.ComposerWidth == ShellLayoutMetrics.ComposerWidthDefault && defaults.ComposerHeight == ShellLayoutMetrics.ComposerHeightDefault,
    "Saved-layout defaults must use the same dimensions as the UI.");
Console.WriteLine("PASS: compact composer bounds, connected-footer height budget, side-by-side and overlay panel geometry, scrollable sidebar row budgets, non-finite measurements and preference restoration.");
