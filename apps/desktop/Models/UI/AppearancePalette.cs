namespace KYNXA_Desktop.Models.UI;

/// <summary>
/// Semantic colors shared by native chrome and the transcript, independent of model APIs.
/// 原生界面与聊天区共用的语义颜色，与模型接口无关。
/// </summary>
public sealed record AppearancePalette(
    string Id, string NameKey, string Accent, string AccentHover, string AccentPressed,
    string Soft, string Sidebar, string Main, string Border, string Selection)
{
    public const string DefaultId = "mist-blue";
    public const string DefaultMode = "light";
    public const double DefaultChatFontSize = 15;
    public const double MinimumChatFontSize = 13;
    public const double MaximumChatFontSize = 22;

    // Preset identity stays independent from the effective system theme and accessible colors.
    // 预设身份独立于当前系统主题与无障碍配色，切换模式不会改写用户选择的预设。
    public bool IsDark { get; init; }
    public bool IsHighContrast { get; init; }
    public string Text { get; init; } = "#24272D";
    public string Secondary { get; init; } = "#606772";
    public string Muted { get; init; } = "#6B7280";
    public string AccentText { get; init; } = Accent;
    public string OnAccent { get; init; } = "#FFFFFF";
    public string Success { get; init; } = "#356B4C";
    public string Error { get; init; } = "#A13C30";
    public string Warning { get; init; } = "#806000";
    public string ReplySurface { get; init; } = "#F7F7F7";
    public string ControlHover { get; init; } = "#E9EBEF";
    public string ControlPressed { get; init; } = "#DEE1E7";
    public string Disabled { get; init; } = "#E2E4E8";
    public string DisabledText { get; init; } = "#6B7280";
    public string CodeKeyword { get; init; } = "#8250DF";
    public string CodeString { get; init; } = "#0A3069";
    public string CodeComment { get; init; } = "#576F43";
    public string CodeNumber { get; init; } = "#0550AE";
    public string CodeType { get; init; } = "#116329";
    public string CodeFunction { get; init; } = "#953800";
}
