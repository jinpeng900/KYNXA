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
    public static double Dimension(string key) => (double)Application.Current.Resources[key];

    public static Flyout Create(FlyoutPlacementMode placement, string style = "KynxaFlyoutPresenterStyle") => new()
    {
        Placement = placement, FlyoutPresenterStyle = (Style)Application.Current.Resources[style]
    };

    public static ListView CreateList(string id, string name, string style = "KynxaModelListItemStyle",
        ListViewSelectionMode selection = ListViewSelectionMode.Single)
    {
        var list = new ListView
        {
            IsItemClickEnabled = true, SelectionMode = selection, MinHeight = 0, Padding = new Thickness(0),
            ItemContainerStyle = (Style)Application.Current.Resources[style]
        };
        list.Resources["ListViewItemSelectionIndicatorVisualEnabled"] = false;
        ScrollViewer.SetHorizontalScrollMode(list, ScrollMode.Disabled);
        ScrollViewer.SetHorizontalScrollBarVisibility(list, ScrollBarVisibility.Disabled);
        ScrollViewer.SetVerticalScrollMode(list, ScrollMode.Enabled);
        ScrollViewer.SetVerticalScrollBarVisibility(list, ScrollBarVisibility.Auto);
        AutomationProperties.SetAutomationId(list, id);
        AutomationProperties.SetName(list, name);
        return list;
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
