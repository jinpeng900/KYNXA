using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Keeps page scrolling from changing a closed selection control.
/// 防止滚动页面时意外修改已收起下拉框的选项。
/// </summary>
public sealed class ScrollSafeComboBox : ComboBox
{
    private ScrollViewer? _dropdownScrollViewer;
    private Thickness _dropdownBasePadding;

    public ScrollSafeComboBox()
    {
        DefaultStyleKey = typeof(ComboBox);
        DropDownOpened += (_, _) => BeginDropdownLayout();
        DropDownClosed += (_, _) => EndDropdownLayout();
        Unloaded += (_, _) => EndDropdownLayout();
    }

    protected override void OnApplyTemplate()
    {
        EndDropdownLayout();
        base.OnApplyTemplate();
        if (IsDropDownOpen) BeginDropdownLayout();
    }

    private void BeginDropdownLayout()
    {
        EndDropdownLayout();
        _dropdownScrollViewer = GetTemplateChild("ScrollViewer") as ScrollViewer;
        if (_dropdownScrollViewer is null) return;
        _dropdownBasePadding = _dropdownScrollViewer.Padding;
        LayoutUpdated += DropdownLayoutUpdated;
        _dropdownScrollViewer.UpdateLayout();
        UpdateDropdownGutter();
    }

    private void DropdownLayoutUpdated(object? sender, object args) => UpdateDropdownGutter();

    private void UpdateDropdownGutter()
    {
        if (!IsDropDownOpen || _dropdownScrollViewer is not { } scroll) return;
        bool hasScrollbar = scroll.ViewportHeight > 0 && scroll.ScrollableHeight > 1 &&
            scroll.ComputedVerticalScrollBarVisibility == Visibility.Visible;
        double gutter = hasScrollbar ? (double)Application.Current.Resources["KynxaComboBoxScrollbarGutter"] : 0;
        var padding = new Thickness(_dropdownBasePadding.Left, _dropdownBasePadding.Top,
            _dropdownBasePadding.Right + gutter, _dropdownBasePadding.Bottom);
        // The SDK binds content padding separately from its overlay scrollbar; virtualized rows need no local edits.
        // SDK 将内容内边距与叠加滚动条分开绑定，无需逐一修改虚拟化选项容器。
        if (scroll.Padding != padding) scroll.Padding = padding;
    }

    private void EndDropdownLayout()
    {
        LayoutUpdated -= DropdownLayoutUpdated;
        var scroll = _dropdownScrollViewer;
        _dropdownScrollViewer = null;
        if (scroll is not null) scroll.Padding = _dropdownBasePadding;
    }

    protected override void OnPointerWheelChanged(PointerRoutedEventArgs args)
    {
        // Skip ComboBox's selection handler while closed. Leaving the routed event unhandled
        // allows the owning ScrollViewer to scroll naturally, including its wheel settings.
        // 收起时跳过 ComboBox 的选项切换逻辑，让未处理的路由事件自然传给所属 ScrollViewer，
        // 保留系统的滚轮距离设置与页面滚动。展开列表及键盘操作继续使用原生行为。
        if (IsDropDownOpen) base.OnPointerWheelChanged(args);
    }
}
