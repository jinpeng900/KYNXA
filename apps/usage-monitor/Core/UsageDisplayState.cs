namespace Kynxa.UsageMonitor.Core;

/// <summary>
/// Carries the owner's already formatted projection; the companion never owns a second usage ledger.
/// 传输宿主已格式化的展示投影，悬浮卡不拥有第二份用量累计。
/// </summary>
public sealed record UsageDisplayState
{
    public string TotalText { get; init; } = "0";
    public string CacheText { get; init; } = "—";
    public string InputText { get; init; } = "—";
    public string OutputText { get; init; } = "—";
    public string CacheHitText { get; init; } = "—";
    public string Description { get; init; } = "";
    public bool IsIncomplete { get; init; }
    public string ScopeLabel { get; init; } = "本次运行";
    public string TotalLabel { get; init; } = "Token 消耗";
    public string CacheLabel { get; init; } = "缓存命中率";
    public string Accent { get; init; } = "#4568B2";
    public string Surface { get; init; } = "#FFFFFF";
    public string Text { get; init; } = "#20242C";
    public string Secondary { get; init; } = "#687181";
    public string Border { get; init; } = "#DCE3EF";
    public string CollapseLabel { get; init; } = "折叠";
    public string ExpandLabel { get; init; } = "展开";
    public string ResetPositionLabel { get; init; } = "恢复右下角";
    public string TopmostLabel { get; init; } = "置顶";
    public string CloseLabel { get; init; } = "关闭";
}
