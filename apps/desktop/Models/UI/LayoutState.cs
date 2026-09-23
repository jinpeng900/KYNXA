namespace KYNXA_Desktop.Models.UI;

public sealed class LayoutState
{
    public const int CurrentVersion = 4;

    public int LayoutVersion { get; set; } = CurrentVersion;
    public double SidebarWidth { get; set; } = 240;
    public bool SidebarCollapsed { get; set; }
    public double TopWorkspaceHeight { get; set; } = 96;
    public bool PreviewVisible { get; set; } = true;
    // Zero keeps the right panel proportional until the user resizes it.
    public double PreviewWidth { get; set; }
    public string PermissionMode { get; set; } = "ask";
    public double ComposerWidth { get; set; } = 824;
    public double ComposerHeight { get; set; } = 110;
    public string LastPrimaryContent { get; set; } = "chat";

    public static LayoutState CreateDefault() => new();
}
