using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Keeps page scrolling from changing a closed selection control.
/// 防止滚动页面时意外修改已收起下拉框的选项。
/// </summary>
public sealed class ScrollSafeComboBox : ComboBox
{
    public ScrollSafeComboBox() => DefaultStyleKey = typeof(ComboBox);

    protected override void OnPointerWheelChanged(PointerRoutedEventArgs args)
    {
        // Skip ComboBox's selection handler while closed. Leaving the routed event unhandled
        // allows the owning ScrollViewer to scroll naturally, including its wheel settings.
        // 收起时跳过 ComboBox 的选项切换逻辑，让未处理的路由事件自然传给所属 ScrollViewer，
        // 保留系统的滚轮距离设置与页面滚动。展开列表及键盘操作继续使用原生行为。
        if (IsDropDownOpen) base.OnPointerWheelChanged(args);
    }
}
