using KYNXA_Desktop.Layout;

namespace KYNXA_Desktop.Models.UI;

public sealed class LayoutState
{
    public const int CurrentVersion = 4;

    public int LayoutVersion { get; set; } = CurrentVersion;
    public double SidebarWidth { get; set; } = ShellLayoutMetrics.SidebarDefault;
    public bool SidebarCollapsed { get; set; }
    public bool WorkRecentExpanded { get; set; }
    // Zero uses the natural navigation height; a dragged divider saves a proportion.
    // 零表示使用自然导航高度；用户拖动分隔线后保存比例。
    public double WorkNavigationRatio { get; set; }
    // Relative to the recent/project content budget; zero keeps natural list heights.
    // 比例相对于最近与项目的内容可用高度；零表示保留列表自然高度。
    public double WorkRecentRatio { get; set; }
    public List<Guid> RecentWorkChatIds { get; set; } = [];
    public double TopWorkspaceHeight { get; set; } = ShellLayoutMetrics.TopDefault;
    public bool PreviewVisible { get; set; } = true;
    // Zero keeps the right panel proportional until the user resizes it.
    // 用户未调整宽度时，零表示右侧面板按比例布局。
    public double PreviewWidth { get; set; }
    public string PermissionMode { get; set; } = "ask";
    public string InterfaceLanguage { get; set; } = "zh-CN";
    public double ComposerWidth { get; set; } = ShellLayoutMetrics.ComposerWidthDefault;
    public double ComposerHeight { get; set; } = ShellLayoutMetrics.ComposerHeightDefault;
    public string LastPrimaryContent { get; set; } = "chat";

    public static LayoutState CreateDefault() => new();
}
