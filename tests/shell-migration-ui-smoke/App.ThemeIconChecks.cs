using System.Globalization;
using System.Text.Json;
using System.Xml.Linq;
using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using PathShape = Microsoft.UI.Xaml.Shapes.Path;

namespace KYNXA_Desktop;

public partial class App
{
    private static Canvas IconCanvas(ThemeIcon icon) => (icon.Content as Viewbox)?.Child as Canvas
        ?? throw new InvalidOperationException("A production theme icon has no native vector canvas.");

    private void CheckThemeIcons(string label)
    {
        var icons = NativeUi.Descendants<ThemeIcon>(_shell).ToArray();
        var shared = AppearanceService.GetBrush("KynxaIconBrush");
        var selected = Element<ThemeIcon>("SelectedPermissionIcon");
        Check(icons.Length >= 10 && icons.All(icon => icon.Width == icon.Height && icon.Width is 16 or 18 or 20 &&
            icon.IsHitTestVisible == false && IconCanvas(icon).Children.Count > 0),
            label + " retains native 16/18/20 DIP vector icons throughout the actual Shell");
        Check(icons.All(icon => IconCanvas(icon).Children.Cast<PathShape>().All(path =>
            path.Data is not null && (path.Stroke is not null || path.Fill is not null) &&
            (path.Stroke is null || ReferenceEquals(path.Stroke, icon.Foreground)) &&
            (path.Fill is null || ReferenceEquals(path.Fill, icon.Foreground)) &&
            path.StrokeStartLineCap == PenLineCap.Round && path.StrokeEndLineCap == PenLineCap.Round &&
            path.StrokeLineJoin == PenLineJoin.Round)) &&
            icons.Where(icon => !ReferenceEquals(icon, selected)).All(icon => ReferenceEquals(icon.Foreground, shared)),
            label + " updates the real icon strokes and fills through shared semantic brushes with round original outlines");
        Check(!NativeUi.Descendants<Image>(_shell).Any(image => image.Source is SvgImageSource source &&
                source.UriSource?.AbsolutePath.Contains("/Resources/UI/Icons/", StringComparison.OrdinalIgnoreCase) == true) &&
            Element<Image>("KynxaLogo").Source is BitmapImage,
            label + " removes fixed-color Shell SVG images while preserving the bitmap brand artwork");

        // Compare actual book geometry to the original bundled asset, not to a second copy of the catalog.
        // 将真实书本图标与随包原始资源对照，避免只验证固定目录的另一份副本。
        var knowledge = icons.Single(icon => icon.IconKey == "IconKnowledge");
        var canvas = IconCanvas(knowledge);
        var book = canvas.Children.Cast<PathShape>().Single();
        var original = XDocument.Load(Path.Combine(AppContext.BaseDirectory, "Resources", "UI", "Icons", "knowledge.svg"));
        var sourcePath = original.Root!.Elements().Single();
        double sourceStroke = double.Parse(original.Root.Attribute("stroke-width")!.Value, CultureInfo.InvariantCulture);
        var bookBounds = book.Data.Bounds;
        bool sourceMatches = ThemeIconGeometry.Icons["IconKnowledge"].Paths.Single().Data == sourcePath.Attribute("d")!.Value;
        bool strokeMatches = Math.Abs(book.StrokeThickness - sourceStroke) < 0.000001;
        string evidenceLabel = new(label.Select(character => char.IsLetterOrDigit(character) ? character : '-').ToArray());
        File.WriteAllText(Path.Combine(_directory, "shell-theme-icons-" + evidenceLabel + ".json"), JsonSerializer.Serialize(new
        {
            Label = label, CanvasWidth = canvas.Width, CanvasHeight = canvas.Height,
            IconWidth = knowledge.Width, IconHeight = knowledge.Height,
            SourceStrokeThickness = sourceStroke, ActualStrokeThickness = book.StrokeThickness,
            StrokeDifference = book.StrokeThickness - sourceStroke, StrokeMatches = strokeMatches,
            FillIsNull = book.Fill is null, SourcePathMatches = sourceMatches,
            GeometryBounds = new { bookBounds.X, bookBounds.Y, bookBounds.Width, bookBounds.Height }
        }, new JsonSerializerOptions { WriteIndented = true }));
        // Native Shape stores stroke thickness as a float even though its projected property is double.
        // 原生 Shape 内部用 float 保存线宽，即使投影属性是 double；仅允许 1e-6 单位的回读精度差。
        Check(canvas.Width == 24 && canvas.Height == 24 && knowledge.Width == 18 &&
            sourceStroke == 1.8 && ThemeIconGeometry.Icons["IconKnowledge"].Paths.Single().StrokeWidth == sourceStroke &&
            strokeMatches && book.Fill is null && sourceMatches &&
            Math.Abs(bookBounds.Width - 17) < 0.001 && bookBounds.Height > 14,
            label + " preserves the original 24-unit book shape, 1.8-unit stroke and 18 DIP layout");
        var connect = icons.Single(icon => icon.IconKey == "IconConnect");
        var dot = IconCanvas(connect).Children.Cast<PathShape>().Single(path => path.Fill is not null);
        var originalConnect = XDocument.Load(Path.Combine(AppContext.BaseDirectory, "Resources", "UI", "Icons", "connect.svg"));
        var sourceCircle = originalConnect.Root!.Elements().Single(element => element.Name.LocalName == "circle");
        double sourceRadius = double.Parse(sourceCircle.Attribute("r")!.Value, CultureInfo.InvariantCulture);
        double sourceCenterX = double.Parse(sourceCircle.Attribute("cx")!.Value, CultureInfo.InvariantCulture);
        double sourceCenterY = double.Parse(sourceCircle.Attribute("cy")!.Value, CultureInfo.InvariantCulture);
        var dotBounds = dot.Data.Bounds;
        var dotDefinition = ThemeIconGeometry.Icons["IconConnect"].Paths.Single(definition => definition.Filled);
        // Record each native observation before asserting so geometry and brush failures remain distinguishable.
        // 在断言前分别记录原生观察值，避免把几何边界偏差与画刷引用失败混为一谈。
        File.WriteAllText(Path.Combine(_directory, "shell-theme-connect-" + evidenceLabel + ".json"), JsonSerializer.Serialize(new
        {
            Label = label,
            SourceCenterX = sourceCenterX, SourceCenterY = sourceCenterY,
            SourceRadius = sourceRadius, SourceDiameter = sourceRadius * 2,
            SourceStroke = sourceCircle.Attribute("stroke")!.Value,
            CatalogData = dotDefinition.Data, dotDefinition.Stroked, dotDefinition.Filled,
            GeometryType = dot.Data.GetType().FullName,
            StrokeIsNull = dot.Stroke is null, FillMatchesSharedBrush = ReferenceEquals(dot.Fill, shared),
            FillMatchesIconForeground = ReferenceEquals(dot.Fill, connect.Foreground),
            FillColor = (dot.Fill as SolidColorBrush)?.Color.ToString(),
            ForegroundMatchesSharedBrush = ReferenceEquals(connect.Foreground, shared),
            GeometryBounds = new { dotBounds.X, dotBounds.Y, dotBounds.Width, dotBounds.Height },
            WidthDifference = dotBounds.Width - sourceRadius * 2,
            HeightDifference = dotBounds.Height - sourceRadius * 2,
            Figures = (dot.Data as PathGeometry)?.Figures.Select(figure => new
            {
                StartPoint = new { figure.StartPoint.X, figure.StartPoint.Y }, figure.IsClosed, figure.IsFilled,
                Segments = figure.Segments.Select(segment => segment is ArcSegment arc ? new
                {
                    Type = segment.GetType().FullName,
                    EndPoint = new { arc.Point.X, arc.Point.Y },
                    Radius = new { arc.Size.Width, arc.Size.Height },
                    arc.RotationAngle, arc.IsLargeArc, SweepDirection = arc.SweepDirection.ToString()
                } : null).ToArray()
            }).ToArray()
        }, new JsonSerializerOptions { WriteIndented = true }));
        Check(sourceRadius == 0.8 && sourceCenterX == 17.5 && sourceCenterY == 13.5 &&
            sourceCircle.Attribute("stroke")!.Value == "none" && sourceCircle.Attribute("fill")!.Value != "none" &&
            dotDefinition.Filled && !dotDefinition.Stroked && dot.Stroke is null && ReferenceEquals(dot.Fill, shared) &&
            Math.Abs(dotBounds.Width - sourceRadius * 2) < 0.001 && Math.Abs(dotBounds.Height - sourceRadius * 2) < 0.001 &&
            Math.Abs(dotBounds.X + dotBounds.Width / 2 - sourceCenterX) < 0.001 &&
            Math.Abs(dotBounds.Y + dotBounds.Height / 2 - sourceCenterY) < 0.001,
            label + " preserves the original filled connection indicator circle as themeable native geometry");

        var layout = ShellField<LayoutState>("_layout");
        string originalMode = layout.PermissionMode;
        try
        {
            foreach (var (mode, key) in new[] { (ChatPermissionModes.Ask, "IconPermissionAsk"),
                (ChatPermissionModes.Smart, "IconPermissionSmart"), (ChatPermissionModes.Full, "IconPermissionFull") })
            {
                layout.PermissionMode = mode;
                Call("UpdatePermissionPickerLabel");
                var brush = AppearanceService.GetBrush(mode == ChatPermissionModes.Full ? "KynxaWarningBrush" : "KynxaSecondaryTextBrush");
                Check(selected.IconKey == key && ReferenceEquals(selected.Foreground, brush) &&
                    IconCanvas(selected).Children.Cast<PathShape>().All(path => ReferenceEquals(path.Stroke, brush)),
                    label + " gives " + mode + " its actual permission shape and semantic foreground");
            }
        }
        finally
        {
            layout.PermissionMode = originalMode;
            Call("UpdatePermissionPickerLabel");
        }

        // Construct every fixed key to cover paths converted from circles, rectangles and polylines.
        // 实例化每个固定键，覆盖由圆、矩形、折线等转换而来的全部路径，防止未出现的图标延迟失败。
        Check(ThemeIcon.KnownKeys.Count() == 26 && ThemeIcon.KnownKeys.All(key =>
        {
            var icon = new ThemeIcon { IconKey = key, Width = 18, Height = 18 };
            return IconCanvas(icon).Children.Cast<PathShape>().All(path => path.Data is not null &&
                double.IsFinite(path.Data.Bounds.X) && double.IsFinite(path.Data.Bounds.Y) &&
                (path.Data.Bounds.Width > 0 || path.Data.Bounds.Height > 0));
        }), label + " parses every built-in fixed vector key as nonempty native geometry");
    }
}
