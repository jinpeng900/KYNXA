using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Foundation;
using KYNXA_Desktop.Services;

namespace KYNXA_Desktop.Controls;

public sealed partial class MarkdownReply
{
    private readonly Canvas _formulaLayer = new() { IsHitTestVisible = false };
    private readonly List<(Run Source, Image Image, FormulaImage Formula, Paragraph Paragraph, bool Block)> _formulas = [];
    private readonly SolidColorBrush _formulaTransparent = new(Colors.Transparent);
    private readonly SolidColorBrush _formulaFallback = new(Colors.Black);
    private readonly Dictionary<Paragraph, double> _formulaBaselines = [];

    private void AddFormula(InlineCollection target, string latex, bool block)
    {
        string source = block ? "$$" + latex + "$$" : "$" + latex + "$";
        var formula = _formulas.Count < 128 ? MathFormulaRenderer.Render(latex, block) : null;
        if (formula is null) { target.Add(new Run { Text = source }); return; }
        var bitmap = new BitmapImage();
        using (var stream = new MemoryStream(formula.Png)) bitmap.SetSource(stream.AsRandomAccessStream());
        var image = new Image { Source = bitmap, Width = formula.Width, Height = formula.Height, Stretch = Stretch.Fill };
        // Native source text retains selection, keyboard copy and screen-reader access.
        // A matching non-interactive image supplies mathematical layout above the native text.
        var measure = new TextBlock { Text = source, FontFamily = CodeFont, FontSize = 14 };
        measure.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
        // Keep the source run at the body font size so its ascent cannot shift the line baseline.
        int spacing = (int)Math.Round((formula.Width - measure.DesiredSize.Width) * 1000 / (14 * Math.Max(1, source.Length)));
        var run = new Run { Text = source, FontFamily = CodeFont, FontSize = 14, CharacterSpacing = spacing,
            Foreground = _formulaTransparent };
        target.Add(run);
        var paragraph = (Paragraph)_document.Blocks.Last();
        paragraph.LineHeight = Math.Max(paragraph.LineHeight > 0 ? paragraph.LineHeight : 23, formula.Height + (block ? 4 : 0));
        _formulas.Add((run, image, formula, paragraph, block));
        _formulaLayer.Children.Add(image);
    }

    private void UpdateFormulas()
    {
        if (!_document.IsLoaded || _document.ActualWidth <= 0) return;
        foreach (var (source, image, formula, paragraph, block) in _formulas)
        {
            var first = source.ContentStart.GetCharacterRect(LogicalDirection.Forward);
            var last = source.ContentEnd.GetCharacterRect(LogicalDirection.Backward);
            bool fits = Math.Abs(first.Y - last.Y) < 1 && last.Right > first.X;
            image.Visibility = fits ? Visibility.Visible : Visibility.Collapsed;
            var foreground = fits ? _formulaTransparent : _formulaFallback;
            if (!ReferenceEquals(source.Foreground, foreground)) source.Foreground = foreground;
            if (!fits) continue;
            image.Width = formula.Width;
            image.Height = formula.Height;
            Canvas.SetLeft(image, first.X);
            // Measure the same paragraph with the same font/line-height. All inline formulas
            // share its baseline; fraction and matrix ascent must not change their alignment.
            if (!_formulaBaselines.TryGetValue(paragraph, out double baseline))
            {
                var baselineProbe = new TextBlock { FontFamily = _document.FontFamily,
                    FontSize = paragraph.FontSize, LineHeight = paragraph.LineHeight,
                    LineStackingStrategy = _document.LineStackingStrategy, Text = "汉Ag" };
                baselineProbe.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
                _formulaBaselines[paragraph] = baseline = baselineProbe.BaselineOffset;
            }
            Canvas.SetTop(image, block ? first.Y + (first.Height - formula.Height) / 2
                : first.Y + baseline - formula.Baseline);
        }
    }
}
