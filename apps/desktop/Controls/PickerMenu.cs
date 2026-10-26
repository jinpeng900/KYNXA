using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Shared menu presentation. Callers retain selection, persistence and action behavior.
/// 共享菜单展示；选择、持久化与具体动作仍由调用方负责。
/// </summary>
internal static class PickerMenu
{
    public const int VisibleRows = 4;
    private const double ViewportInset = 16;
    private const double PresenterChrome = 14;

    public static double Dimension(string key) => (double)Application.Current.Resources[key];

    public static double AvailableContentHeight(double viewportHeight) =>
        Math.Max(0, viewportHeight - ViewportInset * 2 - PresenterChrome);

    public static Flyout Create(FlyoutPlacementMode placement, string style = "KynxaFlyoutPresenterStyle") => new()
    {
        Placement = placement, FlyoutPresenterStyle = (Style)Application.Current.Resources[style]
    };

    public static double SetContentWidth(Flyout flyout, double requestedWidth, double viewportWidth)
    {
        // Match this popup's content plus its existing padding and border; leave shared menu styles unchanged.
        // 匹配当前菜单的内容宽度及既有内边距、边框，不改变共用菜单样式。
        double presenterWidth = Math.Min(requestedWidth + PresenterChrome, Math.Max(0, viewportWidth - ViewportInset * 2));
        var presenterStyle = new Style(typeof(FlyoutPresenter)) { BasedOn = flyout.FlyoutPresenterStyle };
        presenterStyle.Setters.Add(new Setter(FrameworkElement.WidthProperty, presenterWidth));
        flyout.FlyoutPresenterStyle = presenterStyle;
        return Math.Max(0, presenterWidth - PresenterChrome);
    }

    public static ListView CreateList(string id, string name, string style = "KynxaModelListItemStyle",
        ListViewSelectionMode selection = ListViewSelectionMode.Single)
    {
        var list = new ListView
        {
            IsItemClickEnabled = true, SelectionMode = selection, MinHeight = 0, Padding = new Thickness(0),
            ItemContainerStyle = (Style)Application.Current.Resources[style]
        };
        ConfigureListPresentation(list);
        AutomationProperties.SetAutomationId(list, id);
        AutomationProperties.SetName(list, name);
        return list;
    }

    public static double MeasureListHeight(ListView list, double contentWidth, FrameworkElement empty)
    {
        var availableSize = new Windows.Foundation.Size(Math.Max(0, contentWidth), double.PositiveInfinity);
        if (list.Items.Count == 0)
        {
            empty.Measure(availableSize);
            return Math.Max(64, empty.DesiredSize.Height);
        }

        // Measure only the four visible rows, including their native template, padding and margin.
        // 只测量最多四条可见行，计入原生模板、内边距和外边距，不展开整个列表的虚拟化内容。
        double height = list.Padding.Top + list.Padding.Bottom + list.BorderThickness.Top + list.BorderThickness.Bottom;
        var rowSize = new Windows.Foundation.Size(Math.Max(0, contentWidth - list.Padding.Left - list.Padding.Right
            - list.BorderThickness.Left - list.BorderThickness.Right), double.PositiveInfinity);
        for (int index = 0; index < Math.Min(VisibleRows, list.Items.Count); index++)
        {
            // Activate the template's data bindings before measuring an otherwise detached row.
            // 先为模板内容设置数据上下文，避免离树行按空文字和默认可见状态低估高度。
            object item = list.Items[index];
            object content = list.ItemTemplate?.LoadContent() ?? item;
            if (content is FrameworkElement element) element.DataContext = item;
            var row = new ListViewItem
            {
                Content = content, DataContext = item, Style = list.ItemContainerStyle,
                RequestedTheme = list.ActualTheme
            };
            row.ApplyTemplate();
            row.Measure(rowSize);
            height += row.DesiredSize.Height;
        }
        return height;
    }

    // Menus and management lists share native row states without replacing selection or virtualization.
    // 选择菜单与管理列表共用原生行状态，保留各自的选择行为和虚拟化。
    public static void ConfigureListPresentation(ListView list)
    {
        list.Resources["ListViewItemSelectionIndicatorVisualEnabled"] = false;
        // The SDK presenter reads its radius and state colors from the owning list, not the row's CornerRadius.
        // SDK 呈现器从所属列表读取圆角和状态色，不使用行控件本身的 CornerRadius。
        list.Resources["ListViewItemCornerRadius"] = Application.Current.Resources["KynxaControlRadius"];
        foreach ((string target, string source) in new[]
        {
            ("ListViewItemBackgroundPointerOver", "ComboBoxItemBackgroundPointerOver"),
            ("ListViewItemBackgroundPressed", "ComboBoxItemBackgroundPressed"),
            ("ListViewItemForegroundPointerOver", "ComboBoxItemForegroundPointerOver"),
            ("ListViewItemForegroundPressed", "ComboBoxItemForegroundPressed"),
            ("ListViewItemBackgroundSelected", "ComboBoxItemBackgroundSelected"),
            ("ListViewItemBackgroundSelectedPointerOver", "ComboBoxItemBackgroundSelectedPointerOver"),
            ("ListViewItemBackgroundSelectedPressed", "ComboBoxItemBackgroundSelectedPressed"),
            ("ListViewItemForegroundSelected", "ComboBoxItemForegroundSelected")
        })
            list.Resources[target] = Application.Current.Resources[source];
        ScrollViewer.SetHorizontalScrollMode(list, ScrollMode.Disabled);
        ScrollViewer.SetHorizontalScrollBarVisibility(list, ScrollBarVisibility.Disabled);
        ScrollViewer.SetVerticalScrollMode(list, ScrollMode.Enabled);
        ScrollViewer.SetVerticalScrollBarVisibility(list, ScrollBarVisibility.Auto);
    }

    public static Grid WithFixedFooter(FrameworkElement body, FrameworkElement footer, double? height = null)
    {
        var content = new Grid { Width = Dimension("KynxaPickerContentWidth") };
        if (height.HasValue) content.Height = height.Value;
        content.RowDefinitions.Add(new RowDefinition { Height = height.HasValue ? new GridLength(1, GridUnitType.Star) : GridLength.Auto });
        content.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        content.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        content.Children.Add(body);
        var separator = new Border
        {
            Height = 1, Margin = new Thickness(6, 4, 6, 4),
            Background = (Brush)Application.Current.Resources["KynxaDividerBrush"]
        };
        Grid.SetRow(separator, 1);
        content.Children.Add(separator);
        Grid.SetRow(footer, 2);
        content.Children.Add(footer);
        return content;
    }

    public static Button Action(string label, string id, string? glyph = null, double? height = null)
    {
        var button = new Button
        {
            Content = glyph is null ? label : LabelRow(label, glyph),
            Style = (Style)Application.Current.Resources["KynxaMenuActionButtonStyle"]
        };
        if (height.HasValue) button.Height = height.Value;
        AutomationProperties.SetAutomationId(button, id);
        AutomationProperties.SetName(button, label);
        return button;
    }

    public static Grid LabelRow(string label, string glyph)
    {
        var row = new Grid { ColumnSpacing = 8 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(18) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.Children.Add(new FontIcon { Glyph = glyph, FontSize = 16, VerticalAlignment = VerticalAlignment.Center });
        var text = new TextBlock
        {
            Text = label, FontSize = Dimension("KynxaBodyFontSize"), VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis
        };
        Grid.SetColumn(text, 1);
        row.Children.Add(text);
        return row;
    }
}
