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
    public string Text => "#24272D";
    public string Secondary => "#606772";
}
