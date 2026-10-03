using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Controls;

/// <summary>A storage label, current path and explicit folder selection button.</summary>
public sealed class StorageLocationRow : Grid
{
    public TextBlock LocationLabel { get; }
    public TextBlock PathText { get; }
    public Button ChangeButton { get; }

    public StorageLocationRow(string label, string path, string pathAutomationId, string buttonAutomationId)
    {
        var font = (FontFamily)Application.Current.Resources["KynxaUIFont"];
        ColumnSpacing = 14;
        Padding = new Thickness(14, 12, 14, 12);
        CornerRadius = new CornerRadius(12);
        Background = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 247, 247, 247));
        ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        LocationLabel = new TextBlock { FontFamily = font, FontSize = 14, VerticalAlignment = VerticalAlignment.Center };
        UiLocalization.Bind(LocationLabel, TextBlock.TextProperty, label);
        PathText = new TextBlock { FontFamily = font, FontSize = 13, TextWrapping = TextWrapping.NoWrap,
            TextTrimming = TextTrimming.CharacterEllipsis, VerticalAlignment = VerticalAlignment.Center, Opacity = 0.7 };
        AutomationProperties.SetAutomationId(PathText, pathAutomationId);
        ChangeButton = new Button { FontFamily = font, FontSize = 13, Padding = new Thickness(10, 5, 10, 5),
            CornerRadius = new CornerRadius(8), VerticalAlignment = VerticalAlignment.Center,
            Style = (Style)Application.Current.Resources["KynxaQuietButtonStyle"] };
        UiLocalization.Bind(ChangeButton, ContentControl.ContentProperty, "更改位置");
        AutomationProperties.SetAutomationId(ChangeButton, buttonAutomationId);
        Grid.SetColumn(PathText, 1);
        Grid.SetColumn(ChangeButton, 2);
        Children.Add(LocationLabel); Children.Add(PathText); Children.Add(ChangeButton);
        SetPath(path);
    }

    public void SetPath(string path)
    {
        PathText.Text = path;
        ToolTipService.SetToolTip(PathText, path);
    }
}
