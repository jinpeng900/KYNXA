using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Markup;
using Microsoft.UI.Xaml.Media;
using PathShape = Microsoft.UI.Xaml.Shapes.Path;

namespace KYNXA_Desktop.Controls;

/// <summary>Displays trusted, built-in vector geometry with a shared semantic brush.</summary>
/// <remarks>仅显示内置的固定矢量几何，通过共享语义画刷跟随主题；不读取或解析用户 SVG、HTML。</remarks>
public sealed class ThemeIcon : UserControl
{
    public static readonly DependencyProperty IconKeyProperty = DependencyProperty.Register(
        nameof(IconKey), typeof(string), typeof(ThemeIcon), new PropertyMetadata(string.Empty, IconChanged));
    private readonly Viewbox _viewbox = new() { Stretch = Stretch.Uniform };
    private readonly Canvas _canvas = new();
    private readonly List<(PathShape Path, ThemeIconPath Definition)> _paths = [];

    public ThemeIcon()
    {
        IsHitTestVisible = false;
        IsTabStop = false;
        Foreground = AppearanceService.GetBrush("KynxaIconBrush");
        _viewbox.Child = _canvas;
        Content = _viewbox;
        RegisterPropertyChangedCallback(ForegroundProperty, ForegroundChanged);
    }

    public string IconKey
    {
        get => (string)GetValue(IconKeyProperty);
        set => SetValue(IconKeyProperty, value);
    }

    internal static IEnumerable<string> KnownKeys => ThemeIconGeometry.Icons.Keys;

    private static void IconChanged(DependencyObject sender, DependencyPropertyChangedEventArgs args)
        => ((ThemeIcon)sender).BuildGeometry();

    private static void ForegroundChanged(DependencyObject sender, DependencyProperty property)
        => ((ThemeIcon)sender).ApplyBrush();

    private void BuildGeometry()
    {
        _canvas.Children.Clear();
        _paths.Clear();
        if (string.IsNullOrEmpty(IconKey)) return;
        if (!ThemeIconGeometry.Icons.TryGetValue(IconKey, out var icon))
            throw new ArgumentException("Unknown built-in icon key.", nameof(IconKey));
        _canvas.Width = icon.Width;
        _canvas.Height = icon.Height;
        foreach (var definition in icon.Paths)
        {
            // The mini-language comes only from the fixed catalog generated from repository-owned assets.
            // 几何简写只来自仓库内受信图标生成的固定目录，调用者只能选择键，不能传入几何或标记。
            var path = (PathShape)XamlReader.Load(
                "<Path xmlns=\"http://schemas.microsoft.com/winfx/2006/xaml/presentation\" Data=\"" + definition.Data + "\" />");
            path.StrokeThickness = definition.StrokeWidth;
            path.StrokeStartLineCap = PenLineCap.Round;
            path.StrokeEndLineCap = PenLineCap.Round;
            path.StrokeLineJoin = PenLineJoin.Round;
            _paths.Add((path, definition));
            _canvas.Children.Add(path);
        }
        ApplyBrush();
    }

    private void ApplyBrush()
    {
        Brush brush = Foreground ?? AppearanceService.GetBrush("KynxaIconBrush");
        foreach (var (path, definition) in _paths)
        {
            path.Stroke = definition.Stroked ? brush : null;
            path.Fill = definition.Filled ? brush : null;
        }
    }
}

internal sealed record ThemeIconPath(string Data, double StrokeWidth, bool Stroked, bool Filled);
internal sealed record ThemeIconDefinition(double Width, double Height, ThemeIconPath[] Paths);
