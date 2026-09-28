using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;

namespace KYNXA_Desktop.Controls;

public sealed partial class MarkdownReply
{
    // Decorations live behind the single text surface, so copying can cross block boundaries.
    private readonly Canvas _backgrounds = new() { IsHitTestVisible = false };
    private readonly List<(TextElement Element, bool Block)> _backgroundRanges = [];
    private readonly Brush _blockBackground = Application.Current.Resources.TryGetValue("KynxaReplySurfaceBrush", out var background)
        && background is Brush brush ? brush : new SolidColorBrush(Windows.UI.Color.FromArgb(255, 247, 247, 247));
    private readonly SolidColorBrush _inlineBackground = new(Windows.UI.Color.FromArgb(255, 240, 240, 240));
    private bool _backgroundsDirty;

    private void UpdateBackgrounds()
    {
        if (!_backgroundsDirty || !_document.IsLoaded || _document.ActualWidth <= 0) return;
        _backgroundsDirty = false;
        _backgrounds.Children.Clear();
        foreach (var (element, block) in _backgroundRanges)
        {
            var start = element.ContentStart;
            var end = element.ContentEnd;
            if (block)
            {
                var first = start.GetCharacterRect(LogicalDirection.Forward);
                var last = end.GetCharacterRect(LogicalDirection.Backward);
                double left = Math.Max(0, ((Paragraph)element).Margin.Left - 12);
                AddBackground(new Rect(left, Math.Max(0, first.Y - 6),
                    Math.Max(0, _document.ActualWidth - left), Math.Max(first.Height, last.Bottom - first.Y) + 12), true);
            }
            else
            {
                Rect? line = null;
                for (int offset = 0; offset < end.Offset - start.Offset; offset++)
                {
                    var pointer = start.GetPositionAtOffset(offset, LogicalDirection.Forward);
                    if (pointer is null) continue;
                    var rect = pointer.GetCharacterRect(LogicalDirection.Forward);
                    var next = pointer.GetPositionAtOffset(1, LogicalDirection.Forward)?.GetCharacterRect(LogicalDirection.Backward) ?? rect;
                    rect.Width = Math.Max(rect.Width, Math.Abs(next.Y - rect.Y) < 1 ? next.Right - rect.Left : 0);
                    if (rect.Height <= 0) continue;
                    if (line is { } previous && Math.Abs(previous.Y - rect.Y) < 1)
                        line = new Rect(Math.Min(previous.X, rect.X), previous.Y,
                            Math.Max(previous.Right, rect.Right) - Math.Min(previous.X, rect.X), Math.Max(previous.Height, rect.Height));
                    else
                    {
                        if (line is { } finished) AddBackground(finished, false);
                        line = rect;
                    }
                }
                if (line is { } final) AddBackground(final, false);
            }
        }
    }

    private void AddBackground(Rect rect, bool block)
    {
        if (rect.Width <= 0 || rect.Height <= 0) return;
        var border = new Border
        {
            Width = rect.Width, Height = rect.Height, CornerRadius = new CornerRadius(block ? 8 : 3),
            Background = block ? _blockBackground : _inlineBackground,
        };
        Canvas.SetLeft(border, rect.X);
        Canvas.SetTop(border, rect.Y);
        _backgrounds.Children.Add(border);
    }
}
