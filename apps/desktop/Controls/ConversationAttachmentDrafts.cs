using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Compact selectable-file cards stay inside the composer and never write conversation data.
/// 紧凑附件卡片留在输入框内部，不直接写入正式聊天数据。
/// </summary>
public sealed class ConversationAttachmentDrafts : Grid
{
    private readonly StackPanel _items = new() { Orientation = Orientation.Horizontal, Spacing = 8 };
    public event EventHandler<MessageAttachmentDraft>? RemoveRequested;

    public ConversationAttachmentDrafts()
    {
        Children.Add(new ScrollViewer { Content = _items, HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
            VerticalScrollBarVisibility = ScrollBarVisibility.Disabled, HorizontalScrollMode = ScrollMode.Enabled,
            VerticalScrollMode = ScrollMode.Disabled });
        MaxHeight = 118;
        Margin = new Thickness(14, 10, 14, 0);
        Visibility = Visibility.Collapsed;
    }

    public void ShowFiles(IReadOnlyList<MessageAttachmentDraft> files)
    {
        _items.Children.Clear();
        foreach (var file in files)
        {
            var card = new Grid { Width = file.Thumbnail is null ? 174 : 112, Height = 96 };
            var surface = new Border
            {
                CornerRadius = new CornerRadius(10), BorderThickness = new Thickness(1),
                BorderBrush = Brush("KynxaDividerBrush"), Background = Brush("KynxaReplySurfaceBrush"), Child = card
            };
            ToolTipService.SetToolTip(surface, file.Name);
            AutomationProperties.SetName(surface, file.Name);
            if (file.Thumbnail is not null)
                card.Children.Add(new Border { CornerRadius = new CornerRadius(9), Child = new Image
                    { Source = file.Thumbnail, Stretch = Stretch.UniformToFill, Height = 62, VerticalAlignment = VerticalAlignment.Top } });
            else
                card.Children.Add(new FontIcon { Glyph = "\uE8A5", FontSize = 26, Foreground = Brush("KynxaSecondaryTextBrush"),
                    HorizontalAlignment = HorizontalAlignment.Left, Margin = new Thickness(12, 12, 0, 0), VerticalAlignment = VerticalAlignment.Top });
            card.Children.Add(new TextBlock { Text = file.Name, FontSize = 11, TextTrimming = TextTrimming.CharacterEllipsis,
                Foreground = Brush("KynxaTextBrush"), Margin = new Thickness(10, 0, 9, 9), VerticalAlignment = VerticalAlignment.Bottom });
            var remove = new Button { Content = new FontIcon { Glyph = "\uE711", FontSize = 10 }, Padding = new Thickness(4),
                MinWidth = 22, MinHeight = 22, CornerRadius = new CornerRadius(7), Margin = new Thickness(0, 4, 4, 0),
                HorizontalAlignment = HorizontalAlignment.Right, VerticalAlignment = VerticalAlignment.Top,
                Foreground = Brush("KynxaSecondaryTextBrush"), Background = Brush("KynxaReplySurfaceBrush"), BorderThickness = new Thickness(0),
                FocusVisualPrimaryBrush = Brush("KynxaSecondaryTextBrush"), FocusVisualSecondaryBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent) };
            if (Application.Current.Resources.TryGetValue("KynxaCompactIconButtonStyle", out var style) && style is Style buttonStyle)
                remove.Style = buttonStyle;
            AutomationProperties.SetName(remove, UiText.Get("移除附件") + " " + file.Name);
            ToolTipService.SetToolTip(remove, UiText.Get("移除附件"));
            remove.Click += (_, _) => RemoveRequested?.Invoke(this, file);
            card.Children.Add(remove);
            _items.Children.Add(surface);
        }
        Visibility = files.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
    }

    private static Brush Brush(string key) => (Brush)Application.Current.Resources[key];
}
