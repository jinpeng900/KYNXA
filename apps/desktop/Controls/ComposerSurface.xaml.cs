using KYNXA_Desktop.Layout;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// A resizable editor surface with an optional connected, full-width footer.
/// 可调整大小的编辑区，支持连接到下方的全宽页脚。
/// </summary>
public sealed partial class ComposerSurface : UserControl
{
    public static readonly DependencyProperty BodyProperty = DependencyProperty.Register(
        nameof(Body), typeof(object), typeof(ComposerSurface), new PropertyMetadata(null));
    public static readonly DependencyProperty FooterProperty = DependencyProperty.Register(
        nameof(Footer), typeof(object), typeof(ComposerSurface), new PropertyMetadata(null));
    public static readonly DependencyProperty EditorHeightProperty = DependencyProperty.Register(
        nameof(EditorHeight), typeof(double), typeof(ComposerSurface), new PropertyMetadata(ShellLayoutMetrics.ComposerHeightDefault));
    public static readonly DependencyProperty IsFooterVisibleProperty = DependencyProperty.Register(
        nameof(IsFooterVisible), typeof(bool), typeof(ComposerSurface), new PropertyMetadata(false, OnFooterVisibilityChanged));

    public object? Body { get => GetValue(BodyProperty); set => SetValue(BodyProperty, value); }
    public object? Footer { get => GetValue(FooterProperty); set => SetValue(FooterProperty, value); }
    public double EditorHeight { get => (double)GetValue(EditorHeightProperty); set => SetValue(EditorHeightProperty, value); }
    public bool IsFooterVisible { get => (bool)GetValue(IsFooterVisibleProperty); set => SetValue(IsFooterVisibleProperty, value); }
    public double FooterHeight => IsFooterVisible ? FooterSurface.Height : 0;
    public double SurfaceHeight => EditorHeight + FooterHeight;

    public ComposerSurface()
    {
        InitializeComponent();
        UpdateFooterVisibility();
        GotFocus += (_, _) => UpdateFocusBorder();
        LostFocus += (_, _) => DispatcherQueue.TryEnqueue(UpdateFocusBorder);
    }

    private void UpdateFocusBorder()
    {
        bool editorFocused = false;
        if (XamlRoot is not null)
        {
            for (var element = Microsoft.UI.Xaml.Input.FocusManager.GetFocusedElement(XamlRoot) as DependencyObject;
                element is not null; element = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetParent(element))
            {
                if (element == EditorSurface) { editorFocused = true; break; }
            }
        }
        EditorSurface.BorderBrush = AppearanceService.GetBrush(editorFocused ? "KynxaFocusBrush" : "KynxaComposerBorderBrush");
    }

    private static void OnFooterVisibilityChanged(DependencyObject sender, DependencyPropertyChangedEventArgs args) =>
        ((ComposerSurface)sender).UpdateFooterVisibility();

    private void UpdateFooterVisibility()
    {
        if (FooterSurface is null) return;
        FooterSurface.Visibility = FooterBackground.Visibility = IsFooterVisible ? Visibility.Visible : Visibility.Collapsed;
    }
}
