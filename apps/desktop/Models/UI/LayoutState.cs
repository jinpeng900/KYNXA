using KYNXA_Desktop.Layout;

namespace KYNXA_Desktop.Models.UI;

public sealed class LayoutState
{
    public const int CurrentVersion = 4;

    public int LayoutVersion { get; set; } = CurrentVersion;
    public double SidebarWidth { get; set; } = ShellLayoutMetrics.SidebarDefault;
    public bool SidebarCollapsed { get; set; }
    public bool WorkRecentExpanded { get; set; }
    public bool WorkTasksExpanded { get; set; } = true;
    public bool ChatHistoryExpanded { get; set; } = true;
    // Zero uses four visible rows; a drag saves a share of the available list space.
    // 零表示默认四行；拖动后记录占可用列表空间的比例。
    public double WorkRecentHeightRatio { get; set; }
    public double WorkProjectsHeightRatio { get; set; }
    public double WorkTasksHeightRatio { get; set; }
    public List<Guid> RecentWorkChatIds { get; set; } = [];
    public double TopWorkspaceHeight { get; set; } = ShellLayoutMetrics.TopDefault;
    public bool PreviewVisible { get; set; } = true;
    // Zero keeps the right panel proportional until the user resizes it.
    // 用户未调整宽度时，零表示右侧面板按比例布局。
    public double PreviewWidth { get; set; }
    // Zero follows the adaptive default; the remembered ratio never consumes the file viewport.
    // 零使用自适应默认值；保存的任务比例仍为下方文件视图保留空间。
    public double WorkTaskRatio { get; set; }
    public string PermissionMode { get; set; } = "ask";
    public string InterfaceLanguage { get; set; } = "zh-CN";
    public string MainWindowCloseAction { get; set; } = MainWindowClosePreferences.Tray;
    // Confirmation is opt-in; X keeps hiding to the tray by default. / 关闭询问需主动开启，默认 X 仍隐藏到托盘。
    public bool ConfirmMainWindowClose { get; set; }
    public string AppearancePaletteId { get; set; } = AppearancePalette.DefaultId;
    public string AppearanceMode { get; set; } = AppearancePalette.DefaultMode;
    public double ChatFontSize { get; set; } = AppearancePalette.DefaultChatFontSize;
    public string? AppearanceAccent { get; set; }
    public double ComposerWidth { get; set; } = ShellLayoutMetrics.ComposerWidthDefault;
    public double ComposerHeight { get; set; } = ShellLayoutMetrics.ComposerHeightDefault;
    public string LastPrimaryContent { get; set; } = "chat";

    public static LayoutState CreateDefault() => new();
}

internal sealed record MainWindowClosePreferences(string Action, bool Confirm)
{
    public const string Tray = "tray";
    public const string Exit = "exit";

    public static string NormalizeAction(string? action) => action == Exit ? Exit : Tray;
}
