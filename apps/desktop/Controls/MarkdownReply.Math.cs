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
    private sealed class FormulaVisual(Run source, string latex, Paragraph paragraph, bool block)
    {
        public Run Source { get; } = source;
        public string Latex { get; } = latex;
        public Paragraph Paragraph { get; } = paragraph;
        public bool Block { get; } = block;
        public Image Image { get; } = new() { Visibility = Visibility.Collapsed, Stretch = Stretch.Fill };
        public FormulaImage? Formula { get; set; }
        public bool Requested { get; set; }
        public bool Ready { get; set; }
        public bool Applied { get; set; }
    }
    private readonly List<FormulaVisual> _formulas = [];
    private readonly SolidColorBrush _formulaTransparent = new(Colors.Transparent);
    private readonly SolidColorBrush _formulaFallback = new(Colors.Black);
    private readonly Dictionary<Paragraph, double> _formulaBaselines = [];
    private DispatcherTimer? _formulaUpdateTimer;
    internal bool HasPendingFormulaRendering => _formulas.Any(formula => !formula.Applied);

    private void AddFormula(InlineCollection target, string latex, bool block)
    {
        string source = block ? "$$" + latex + "$$" : "$" + latex + "$";
        var run = new Run { Text = source, FontFamily = CodeFont, FontSize = 14 };
        target.Add(run);
        // Keep readable source until the local KaTeX worker has a complete image.
        // 本地 KaTeX 完整生成图片之前，保留可阅读的公式源文。
        if (_formulas.Count >= 256) return;
        _formulas.Add(new FormulaVisual(run, latex, (Paragraph)_document.Blocks.Last(), block));
    }

    private void QueueFormulaRendering()
    {
        if (!IsLoaded) return;
        ApplyReadyFormulas();
        foreach (var formula in _formulas.Where(formula => !formula.Requested).ToArray())
        {
            formula.Requested = true;
            _ = ResolveFormulaAsync(formula);
        }
    }

    private async Task ResolveFormulaAsync(FormulaVisual entry)
    {
        FormulaImage? rendered;
        try
        {
            var worker = KatexFormulaRenderer.ForElement(this);
            rendered = worker is null ? null : await worker.RenderAsync(entry.Latex, entry.Block);
        }
        catch (Exception) { rendered = null; }
        // The native renderer remains an offline fallback when WebView2 cannot start.
        // WebView2 无法启动时，原生渲染器继续提供离线回退。
        rendered ??= MathFormulaRenderer.Render(entry.Latex, entry.Block);
        if (!_formulas.Contains(entry)) return; // A changed tail/chat must never receive an old image.
        // 尾部内容或聊天已切换时，不能应用旧任务返回的图片。
        entry.Formula = rendered;
        entry.Ready = true;
        if (IsLoaded) ApplyReadyFormulas();
    }

    private void ApplyReadyFormulas()
    {
        if (!_formulas.Any(formula => formula.Ready && !formula.Applied)) return;
        if (_selectionScroll.HasSelection)
        {
            _formulaUpdateTimer ??= new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(100) };
            _formulaUpdateTimer.Tick -= FormulaUpdateTimer_Tick;
            _formulaUpdateTimer.Tick += FormulaUpdateTimer_Tick;
            _formulaUpdateTimer.Start();
            return;
        }
        _formulaUpdateTimer?.Stop();
        foreach (var entry in _formulas.Where(formula => formula.Ready && !formula.Applied))
        {
            entry.Applied = true;
            if (entry.Formula is not { } formula) continue;
            ApplyFormulaImage(entry, formula);
        }
        _formulaBaselines.Clear();
        _tableLayoutDirty = true;
        _backgroundsDirty = true;
        _document.InvalidateMeasure();
    }

    private void FormulaUpdateTimer_Tick(object? sender, object e) => ApplyReadyFormulas();

    private void ApplyFormulaImage(FormulaVisual entry, FormulaImage formula)
    {
        var bitmap = new BitmapImage();
        using (var stream = new MemoryStream(formula.Png)) bitmap.SetSource(stream.AsRandomAccessStream());
        entry.Image.Source = bitmap;
        entry.Image.Width = formula.Width;
        entry.Image.Height = formula.Height;
        // Native source text retains selection, keyboard copy and screen-reader access.
        // A matching non-interactive image supplies mathematical layout above the native text.
        // 原生源文保留选择、键盘复制与屏幕阅读器能力；对应的非交互图片只补充公式排版。
        var measure = new TextBlock { Text = entry.Source.Text, FontFamily = CodeFont, FontSize = 14 };
        measure.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
        // Keep the source run at the body font size so its ascent cannot shift the line baseline.
        // 源文保持正文字号，避免其上升部改变整行基线。
        entry.Source.CharacterSpacing = (int)Math.Round((formula.Width - measure.DesiredSize.Width) * 1000 / (14 * Math.Max(1, entry.Source.Text.Length)));
        entry.Source.Foreground = _formulaTransparent;
        InvalidateTableFormula(entry.Source);
        var paragraph = entry.Paragraph;
        paragraph.LineHeight = Math.Max(paragraph.LineHeight > 0 ? paragraph.LineHeight : 23, formula.Height + (entry.Block ? 4 : 0));
        // Completion order depends on batching/cache hits; keep the visual tree in source order.
        // 批处理和缓存命中会改变完成顺序；视觉树始终保持源文顺序。
        int imageIndex = _formulas.TakeWhile(formula => !ReferenceEquals(formula, entry))
            .Count(formula => _formulaLayer.Children.Contains(formula.Image));
        _formulaLayer.Children.Insert(imageIndex, entry.Image);
    }

    private void UpdateFormulas()
    {
        if (!_document.IsLoaded || _document.ActualWidth <= 0) return;
        foreach (var entry in _formulas)
        {
            if (!entry.Applied || entry.Formula is not { } formula) continue;
            var (source, image, paragraph, block) = (entry.Source, entry.Image, entry.Paragraph, entry.Block);
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
            // 用同一字体与行高测量同一段落；行内公式共享基线，分数和矩阵的上升部不改变对齐。
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
