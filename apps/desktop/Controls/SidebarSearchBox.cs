using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Keeps narrow sidebar hints inside the native editor without changing its input behavior.
/// 将窄侧栏提示限制在原生输入框内，不改变其编辑行为。
/// </summary>
public sealed class SidebarSearchBox : TextBox
{
    public SidebarSearchBox()
    {
        DefaultStyleKey = typeof(TextBox);
    }

    protected override void OnApplyTemplate()
    {
        base.OnApplyTemplate();
        // The SDK placeholder does not trim by default; retain its template and native input viewport.
        // SDK 占位提示默认不省略；沿用原模板和原生文本滚动视口。
        if (GetTemplateChild("PlaceholderTextContentPresenter") is TextBlock placeholder)
            placeholder.TextTrimming = TextTrimming.CharacterEllipsis;
    }
}
