using KYNXA_Desktop.Layout;

namespace KYNXA_Desktop.Models.UI;

public sealed class LayoutState
{
    public const int CurrentVersion = 4;

    public int LayoutVersion { get; set; } = CurrentVersion;
    public double SidebarWidth { get; set; } = ShellLayoutMetrics.SidebarDefault;
    public bool SidebarCollapsed { get; set; }
    public bool WorkRecentExpanded { get; set; }
    public List<Guid> RecentWorkChatIds { get; set; } = [];
    public double TopWorkspaceHeight { get; set; } = ShellLayoutMetrics.TopDefault;
    public bool PreviewVisible { get; set; } = true;
    // Zero keeps the right panel proportional until the user resizes it.
    public double PreviewWidth { get; set; }
    public string PermissionMode { get; set; } = "ask";
    public double ComposerWidth { get; set; } = ShellLayoutMetrics.ComposerWidthDefault;
    public double ComposerHeight { get; set; } = ShellLayoutMetrics.ComposerHeightDefault;
    public string LastPrimaryContent { get; set; } = "chat";

    public static LayoutState CreateDefault() => new();
}
