using KYNXA_Desktop.Layout;
using KYNXA_Desktop.Models.UI;

static void Check(bool condition, string message)
{
    if (!condition) throw new InvalidOperationException(message);
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
Check(!ShellLayoutMetrics.CanShowWorkPanel(899, 900), "Narrow window should hide the right panel.");
Check(!ShellLayoutMetrics.CanShowWorkPanel(1400, 439), "Short window should hide the right panel.");
Check(ShellLayoutMetrics.CanShowWorkPanel(900, 440), "Right panel should fit at the minimum supported size.");
double preferred = 1400;
double narrow = ShellLayoutMetrics.GetWorkPanelWidth(1000, preferred);
double restored = ShellLayoutMetrics.GetWorkPanelWidth(2200, preferred);
Check(narrow < preferred && restored == preferred, "Temporary window constraints must preserve the user's preferred width.");
foreach (double available in new[] { 260d, 400, 700 })
{
    double editor = ShellLayoutMetrics.GetComposerHeightMaximum(available, 44);
    Check(editor + 44 + 32 <= available, "Connected footer must be included in the height budget.");
}
var defaults = LayoutState.CreateDefault();
Check(defaults.WorkNavigationRatio == 0, "Existing layouts must keep natural sidebar heights until the divider is dragged.");
foreach (double height in new[] { 0d, 80, 180, 400, 900 })
{
    var (minimum, maximum) = ShellLayoutMetrics.GetWorkNavigationHeightRange(height);
    Check(minimum >= 0 && maximum >= minimum && maximum <= Math.Max(0, height - 8), "Sidebar split must remain inside the available height.");
    if (height >= 180) Check(height - 8 - maximum >= 96, "Dragging must retain room for the tasks header and chats.");
}
foreach (var (height, singleMinimum, bothMinimum, maximum) in new[]
{
    (0d, 0d, 0d, 0d), (80d, 0d, 0d, 0d), (180d, 76d, 76d, 76d),
    (240d, 136d, 136d, 136d), (312d, 136d, 208d, 208d),
    (400d, 136d, 208d, 296d), (900d, 136d, 208d, 796d)
})
{
    Check(ShellLayoutMetrics.GetWorkNavigationHeightRange(height) == (singleMinimum, maximum),
        $"One expanded navigation section has incorrect bounds at height {height}.");
    Check(ShellLayoutMetrics.GetWorkNavigationHeightRange(height, bothExpanded: true) == (bothMinimum, maximum),
        $"Two expanded navigation sections have incorrect bounds at height {height}.");
}
foreach (double invalid in new[] { -100d, double.NaN, double.NegativeInfinity, double.PositiveInfinity })
{
    Check(ShellLayoutMetrics.GetWorkNavigationHeightRange(invalid) == (0d, 0d) &&
        ShellLayoutMetrics.GetWorkNavigationHeightRange(invalid, bothExpanded: true) == (0d, 0d),
        "Invalid or negative sidebar heights must produce finite empty bounds.");
    Check(ShellLayoutMetrics.GetWorkRecentHeightRange(invalid) == (0d, 0d),
        "Invalid or negative navigation budgets must produce finite empty bounds.");
}
foreach (var (budget, minimum, maximum) in new[]
{
    (0d, 0d, 0d), (80d, 40d, 40d), (128d, 64d, 64d), (180d, 64d, 116d),
    (400d, 64d, 336d), (900d, 64d, 836d)
})
{
    Check(ShellLayoutMetrics.GetWorkRecentHeightRange(budget) == (minimum, maximum),
        $"Recent/project split has incorrect bounds for budget {budget}.");
    foreach (double ratio in new[] { -double.MaxValue, -1d, 0d, 0.0001, 0.5, 1d, 2d, double.MaxValue })
    {
        double recent = Math.Clamp(budget * ratio, minimum, maximum);
        double projects = budget - recent;
        Check(double.IsFinite(recent) && recent >= 0 && projects >= 0,
            "Extreme inner split ratios must not overflow or allocate negative space.");
        if (budget >= 128) Check(recent >= 64 && projects >= 64,
            "Both expanded lists must retain at least 64 pixels when space permits.");
        else Check(recent == projects, "A constrained navigation budget must be divided evenly.");
    }
}
foreach (double height in new[] { 0d, 80, 180, 400, 900 })
foreach (bool bothExpanded in new[] { false, true })
{
    var (minimum, maximum) = ShellLayoutMetrics.GetWorkNavigationHeightRange(height, bothExpanded);
    foreach (double ratio in new[] { -double.MaxValue, -1d, 0d, 0.5, 1d, 2d, double.MaxValue })
    {
        double navigation = Math.Clamp(height * ratio, minimum, maximum);
        Check(double.IsFinite(navigation) && navigation >= minimum && navigation <= maximum,
            "Extreme outer split ratios must stay inside finite navigation bounds.");
        if (height >= 104) Check(height - 8 - navigation >= 96,
            "Extreme split ratios must retain the task area minimum.");
    }
}
var preferredSplit = new LayoutState { WorkNavigationRatio = 0.7 };
var restoredSplit = System.Text.Json.JsonSerializer.Deserialize<LayoutState>(System.Text.Json.JsonSerializer.Serialize(preferredSplit));
Check(restoredSplit?.WorkNavigationRatio == 0.7, "Sidebar split preference must survive save and restore.");
Check(defaults.ComposerWidth == ShellLayoutMetrics.ComposerWidthDefault && defaults.ComposerHeight == ShellLayoutMetrics.ComposerHeightDefault,
    "Saved-layout defaults must use the same dimensions as the UI.");

WorkSidebarLayoutResult Sidebar(double available, bool recent, bool projects, double navigationRatio = 0, double recentRatio = 0) =>
    WorkSidebarLayout.Calculate(available, recent, projects, navigationRatio, recentRatio)
    ?? throw new InvalidOperationException("A measured sidebar did not produce geometry.");
void Row(WorkSidebarRowHeight row, WorkSidebarRowSizing sizing, double value, string message) =>
    Check(row.Sizing == sizing && row.Value == value, message);

foreach (double invalid in new[] { 0d, -100, double.NaN, double.NegativeInfinity, double.PositiveInfinity })
    Check(WorkSidebarLayout.Calculate(invalid, true, true, 0.7, 0.3) is null,
        "Unmeasured or detached views must leave the applied geometry intact.");
var collapsedSidebar = Sidebar(900, false, false, 0.7, 0.3);
Check(collapsedSidebar.NavigationRow.Sizing == WorkSidebarRowSizing.Auto &&
      collapsedSidebar.RecentRow.Sizing == WorkSidebarRowSizing.Auto &&
      collapsedSidebar.ProjectsRow.Sizing == WorkSidebarRowSizing.Auto &&
      collapsedSidebar.NavigationGripHeight == 0 && collapsedSidebar.RecentGripHeight == 0 &&
      collapsedSidebar.RecentMaximumHeight == 0 && collapsedSidebar.ProjectsMaximumHeight == 0,
    "Collapsing both sections removes divider space and ignores saved proportions without leaving an empty navigation area.");
var automaticSidebar = Sidebar(900, true, true);
Row(automaticSidebar.NavigationRow, WorkSidebarRowSizing.Auto, 0, "Undragged navigation must retain its natural height.");
Row(automaticSidebar.RecentRow, WorkSidebarRowSizing.Auto, 0, "Undragged recent list must retain its natural height.");
Row(automaticSidebar.ProjectsRow, WorkSidebarRowSizing.Star, 1, "Expanded projects must fill the remaining navigation space.");
Check(automaticSidebar.NavigationGripHeight == 8 && automaticSidebar.RecentGripHeight == 8 &&
      automaticSidebar.RecentMaximumHeight == 185 && automaticSidebar.ProjectsMaximumHeight == 185,
    "Both automatic lists share the 450-pixel navigation allowance after two headers and the inner divider.");
var resizedSidebar = Sidebar(900, true, true, 0.7, 0.25);
Row(resizedSidebar.NavigationRow, WorkSidebarRowSizing.Pixels, 630, "An outer drag must apply its saved window-relative proportion.");
Row(resizedSidebar.RecentRow, WorkSidebarRowSizing.Pixels, 137.5, "An inner drag must apply its saved content-relative proportion.");
Check(resizedSidebar.RecentMaximumHeight == 137.5 && double.IsPositiveInfinity(resizedSidebar.ProjectsMaximumHeight),
    "A fixed navigation boundary lets projects fill all remaining space without a second height cap.");
var recentOnly = Sidebar(900, true, false, 0.7, 0.25);
Row(recentOnly.RecentRow, WorkSidebarRowSizing.Star, 1, "A resized recent-only section must fill its navigation allowance.");
Check(recentOnly.NavigationGripHeight == 8 && recentOnly.RecentGripHeight == 0 && recentOnly.RecentMaximumHeight == 558,
    "A single expanded recent section must not reserve an inner divider.");
var projectsOnly = Sidebar(900, false, true);
Check(projectsOnly.RecentRow.Sizing == WorkSidebarRowSizing.Auto && projectsOnly.RecentGripHeight == 0 &&
      projectsOnly.RecentMaximumHeight == 378 && projectsOnly.ProjectsMaximumHeight == 378,
    "A single automatic project section receives the full content allowance.");
Row(Sidebar(900, true, true, 80d / 900).NavigationRow, WorkSidebarRowSizing.Pixels, 80,
    "Freezing a naturally short navigation boundary must not enlarge it to the normal drag minimum.");
foreach (double invalidRatio in new[] { -1d, double.NaN, double.NegativeInfinity, double.PositiveInfinity })
    Check(Sidebar(900, true, true, invalidRatio, invalidRatio) == automaticSidebar,
        "Absent or invalid saved ratios must retain automatic sizing.");
var layoutPreferences = new LayoutState { WorkNavigationRatio = 0.7, WorkRecentRatio = 0.25, SidebarWidth = 271, PreviewWidth = 1200 };
var narrowSidebar = Sidebar(400, true, true, layoutPreferences.WorkNavigationRatio, layoutPreferences.WorkRecentRatio);
var restoredSidebar = Sidebar(900, true, true, layoutPreferences.WorkNavigationRatio, layoutPreferences.WorkRecentRatio);
Check(narrowSidebar.NavigationRow.Value == 280 && restoredSidebar == resizedSidebar &&
      layoutPreferences.WorkNavigationRatio == 0.7 && layoutPreferences.WorkRecentRatio == 0.25 &&
      layoutPreferences.SidebarWidth == 271 && layoutPreferences.PreviewWidth == 1200,
    "Temporary size constraints must restore the saved split and preserve independent width preferences.");

var divider = new WorkSidebarLayout();
var dragPreferences = LayoutState.CreateDefault();
divider.BeginNavigationDrag(450, dragPreferences);
Check(!divider.ResizeNavigation(dragPreferences, 900, true, true, 0.49) && dragPreferences.WorkNavigationRatio == 0,
    "Subpixel outer pointer noise must not create a saved proportion.");
Check(divider.ResizeNavigation(dragPreferences, 900, true, true, 100) && dragPreferences.WorkNavigationRatio == 550d / 900,
    "Outer dragging measures total delta from the initial boundary.");
divider.ResizeNavigation(dragPreferences, 900, true, true, 200);
Check(dragPreferences.WorkNavigationRatio == 650d / 900, "Repeated outer updates must not accumulate previous deltas.");
divider.ResizeNavigation(dragPreferences, 900, true, true, 10000);
Check(dragPreferences.WorkNavigationRatio == 796d / 900, "Outer dragging must retain 96 pixels for tasks plus its divider.");
divider.CancelNavigationDrag(dragPreferences);
Check(dragPreferences.WorkNavigationRatio == 0, "Escape must restore the automatic outer boundary.");
divider.BeginRecentDrag(185, 450, dragPreferences);
Check(!divider.ResizeRecent(dragPreferences, 900, 0.49) && dragPreferences.WorkNavigationRatio == 0 && dragPreferences.WorkRecentRatio == 0,
    "Subpixel inner pointer noise must not freeze the automatic task boundary.");
Check(divider.ResizeRecent(dragPreferences, 900, 45) && dragPreferences.WorkNavigationRatio == 0.5 &&
      dragPreferences.WorkRecentRatio == 230d / 370,
    "Inner dragging freezes the measured outer boundary and uses only the recent/project content budget.");
divider.ResizeRecent(dragPreferences, 900, 90);
Check(dragPreferences.WorkNavigationRatio == 0.5 && dragPreferences.WorkRecentRatio == 275d / 370,
    "Inner dragging keeps the task boundary fixed and uses delta from the original recent height.");
divider.CancelRecentDrag(dragPreferences);
Check(dragPreferences.WorkNavigationRatio == 0 && dragPreferences.WorkRecentRatio == 0,
    "Escape after an inner drag must restore both previous proportions.");
dragPreferences.WorkNavigationRatio = 0.7;
dragPreferences.WorkRecentRatio = 0.25;
divider.BeginRecentDrag(185, 450, dragPreferences);
divider.ResizeRecent(dragPreferences, 900, 45);
Check(dragPreferences.WorkNavigationRatio == 0.7,
    "Inner dragging must preserve an existing outer preference even when current geometry was constrained.");
divider.CancelRecentDrag(dragPreferences);
Check(dragPreferences.WorkNavigationRatio == 0.7 && dragPreferences.WorkRecentRatio == 0.25,
    "Cancellation must restore both explicit preferences exactly.");
divider.ResetRecent(dragPreferences);
Check(dragPreferences.WorkRecentRatio == 0 && dragPreferences.WorkNavigationRatio == 0.7,
    "Resetting the inner divider must leave the task boundary preference intact.");
divider.ResetNavigation(dragPreferences);
Check(dragPreferences.WorkNavigationRatio == 0, "Resetting the outer divider must restore natural height.");
divider.BeginRecentDrag(0, 80, dragPreferences);
Check(!divider.ResizeRecent(dragPreferences, 900, 30) && dragPreferences.WorkNavigationRatio == 0 && dragPreferences.WorkRecentRatio == 0,
    "A sidebar without a list content budget cannot acquire a split preference.");
Console.WriteLine("PASS: composer/right-panel geometry; sidebar row allocation, collapsed sections, saved proportions, divider drag snapshots, cancellation and reset.");
