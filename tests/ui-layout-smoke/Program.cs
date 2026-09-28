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
Check(defaults.ComposerWidth == ShellLayoutMetrics.ComposerWidthDefault && defaults.ComposerHeight == ShellLayoutMetrics.ComposerHeightDefault,
    "Saved-layout defaults must use the same dimensions as the UI.");
Console.WriteLine("PASS: composer bounds, connected-footer height budget, right-panel limits, responsive thresholds and preference restoration.");
