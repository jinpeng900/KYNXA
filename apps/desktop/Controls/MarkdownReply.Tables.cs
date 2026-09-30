using Markdig.Extensions.Tables;
using Markdig.Syntax;
using Microsoft.UI;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;
using Paragraph = Microsoft.UI.Xaml.Documents.Paragraph;
using Span = Microsoft.UI.Xaml.Documents.Span;

namespace KYNXA_Desktop.Controls;

public sealed partial class MarkdownReply
{
    private sealed class TableCellVisual(Span content)
    {
        public Span Content { get; } = content;
        public Run Gap { get; } = new() { Text = " | ", Foreground = new SolidColorBrush(Colors.Transparent), FontSize = 14 };
        public double NaturalWidth { get; set; }
        public bool Multiline { get; set; }
        public bool NeedsMeasure { get; set; } = true;
        public TableVisual? Table { get; set; }
    }
    private sealed record TableRowVisual(Paragraph Paragraph, bool Header, List<TableCellVisual> Cells)
    {
        public (string? Family, double Size, ushort Weight, int Style, int Spacing) MeasureStyle { get; set; }
    }
    private sealed class TableVisual(double indent, List<TableRowVisual> rows)
    {
        public double Indent { get; } = indent;
        public List<TableRowVisual> Rows { get; } = rows;
        public string[] Labels { get; set; } = [];
        public bool Stacked { get; set; }
        public double[] Widths { get; set; } = [];
        public double AvailableWidth { get; set; }
        public int AlignmentPasses { get; set; }
        public bool AlignmentSettled { get; set; }
        public bool NeedsMeasure { get; set; } = true;
        public double[] NaturalWidths { get; set; } = [];
        public bool HasMultilineCells { get; set; }
    }
    private sealed record TableSnapshot(TableVisual Table, double Available, double[] Widths,
        List<(TableRowVisual Row, Rect[] First, Rect[] Last)> Rows, bool Multiline);
    private readonly List<TableVisual> _tables = [];
    private readonly Dictionary<Run, TableCellVisual> _tableRunCells = [];
    internal long TableCellMeasureCount { get; private set; }
    private bool _tableLayoutDirty;
    private DispatcherTimer? _tableLayoutTimer;
    private readonly Brush _tableRule = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 226, 226, 226));

    private void AddTable(Table table, double indent)
    {
        var rows = new List<TableRowVisual>();
        foreach (TableRow row in table)
        {
            var paragraph = NewParagraph(indent + 12);
            paragraph.FontFamily = _document.FontFamily;
            paragraph.FontSize = 14;
            paragraph.LineHeight = 23;
            paragraph.FontWeight = row.IsHeader ? FontWeights.SemiBold : FontWeights.Normal;
            paragraph.Margin = new Thickness(indent + 12, _document.Blocks.Count == 1 ? 8 : 12, 12, 0);
            var cells = new List<TableCellVisual>();
            foreach (TableCell cell in row)
            {
                var span = new Span();
                paragraph.Inlines.Add(span);
                bool first = true;
                foreach (LeafBlock leaf in cell.OfType<LeafBlock>())
                {
                    if (!first) span.Inlines.Add(new LineBreak());
                    first = false;
                    if (leaf.Inline is not null) AddInlines(span.Inlines, leaf.Inline);
                    else span.Inlines.Add(new Run { Text = leaf.Lines.ToString() });
                }
                var visual = new TableCellVisual(span);
                cells.Add(visual);
                paragraph.Inlines.Add(visual.Gap);
            }
            if (cells.Count > 0) paragraph.Inlines.Remove(cells[^1].Gap);
            rows.Add(new(paragraph, row.IsHeader, cells));
        }
        if (rows.Count == 0) return;
        int columns = rows.Max(row => row.Cells.Count);
        var header = rows.FirstOrDefault(row => row.Header);
        var visualTable = new TableVisual(indent, rows)
        {
            Labels = Enumerable.Range(0, columns).Select(index =>
            {
                string label = header is not null && index < header.Cells.Count ? InlineText(header.Cells[index].Content).Trim() : "";
                return label.Length > 0 ? label : $"列 {index + 1}";
            }).ToArray()
        };
        _tables.Add(visualTable);
        foreach (var row in rows)
            foreach (var cell in row.Cells)
            {
                cell.Table = visualTable;
                foreach (var run in TableCellRuns(cell.Content)) _tableRunCells.Add(run, cell);
            }
        _tableLayoutDirty = true;
    }

    private static IEnumerable<Run> TableCellRuns(Span span)
    {
        foreach (var inline in span.Inlines)
            if (inline is Run run) yield return run;
            else if (inline is Span nested)
                foreach (var child in TableCellRuns(nested)) yield return child;
    }

    // Parsed cells are immutable until their Markdown block is replaced. Formula image
    // application is the only in-place text metric change, and invalidates just its cell.
    private void InvalidateTableFormula(Run source)
    {
        if (!_tableRunCells.TryGetValue(source, out var cell)) return;
        cell.NeedsMeasure = true;
        if (cell.Table is { } table)
        {
            table.NeedsMeasure = true;
            table.AlignmentPasses = 0;
            table.AlignmentSettled = false;
        }
        _tableLayoutDirty = true;
    }

    private void MeasureTableCells(TableVisual table)
    {
        foreach (var row in table.Rows)
        {
            var paragraph = row.Paragraph;
            var style = (paragraph.FontFamily.Source, paragraph.FontSize, paragraph.FontWeight.Weight,
                (int)paragraph.FontStyle, paragraph.CharacterSpacing);
            if (row.MeasureStyle == style) continue;
            row.MeasureStyle = style;
            table.NeedsMeasure = true;
            table.AlignmentPasses = 0;
            table.AlignmentSettled = false;
            foreach (var cell in row.Cells) cell.NeedsMeasure = true;
        }
        if (!table.NeedsMeasure) return;
        var widths = new double[table.Labels.Length];
        bool multiline = false;
        foreach (var row in table.Rows)
            for (int i = 0; i < row.Cells.Count; i++)
            {
                var cell = row.Cells[i];
                if (cell.NeedsMeasure)
                {
                    var probe = new TextBlock { FontFamily = _document.FontFamily, FontSize = 14,
                        FontWeight = row.Paragraph.FontWeight, LineHeight = 23, TextWrapping = TextWrapping.NoWrap };
                    probe.Inlines.Add(MeasureInline(cell.Content));
                    probe.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
                    cell.NaturalWidth = probe.DesiredSize.Width;
                    cell.Multiline = probe.DesiredSize.Height > 30;
                    cell.NeedsMeasure = false;
                    TableCellMeasureCount++;
                }
                widths[i] = Math.Max(widths[i], cell.NaturalWidth);
                multiline |= cell.Multiline;
            }
        table.NaturalWidths = widths;
        table.HasMultilineCells = multiline;
        table.NeedsMeasure = false;
    }

    private static string InlineText(Inline inline) => inline switch
    {
        Run run => run.Text,
        LineBreak => " ",
        Span span => string.Concat(span.Inlines.Select(InlineText)),
        _ => ""
    };

    private static Inline MeasureInline(Inline original)
    {
        Inline clone = original switch
        {
            Run run => new Run { Text = run.Text },
            LineBreak => new LineBreak(),
            Span span => CopySpan(span),
            _ => new Run()
        };
        clone.FontFamily = original.FontFamily;
        clone.FontSize = original.FontSize;
        clone.FontWeight = original.FontWeight;
        clone.FontStyle = original.FontStyle;
        clone.CharacterSpacing = original.CharacterSpacing;
        return clone;
    }

    private static Span CopySpan(Span original)
    {
        var clone = new Span();
        foreach (var child in original.Inlines) clone.Inlines.Add(MeasureInline(child));
        return clone;
    }

    private void UpdateTables()
    {
        if (!_tableLayoutDirty || !_document.IsLoaded || ActualWidth <= 0) return;
        if (_selectionScroll.HasSelection)
        {
            if (_tableLayoutTimer is null)
            {
                _tableLayoutTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(100) };
                _tableLayoutTimer.Tick += (_, _) =>
                {
                    if (!IsLoaded || !_tableLayoutDirty) { _tableLayoutTimer.Stop(); return; }
                    if (_selectionScroll.HasSelection) return;
                    _tableLayoutTimer.Stop();
                    UpdateTables();
                    _document.InvalidateMeasure();
                };
            }
            _tableLayoutTimer.Start();
            return;
        }
        _tableLayoutTimer?.Stop();
        _tableLayoutDirty = false;
        _backgroundsDirty = true;
        // Read every cell first. Changing one spacing run invalidates native pointer geometry
        // for later cells; no measurements are taken during the correction pass below.
        var snapshots = new List<TableSnapshot>();
        foreach (var table in _tables)
        {
            MeasureTableCells(table);
            var widths = table.NaturalWidths;
            double available = Math.Max(0, ActualWidth - table.Indent - 24);
            bool stacked = table.HasMultilineCells || widths.Sum() + Math.Max(0, widths.Length - 1) * 24 > available;
            bool targetsChanged = Math.Abs(table.AvailableWidth - available) > 1
                || table.Widths.Length != widths.Length
                || table.Widths.Where((width, index) => Math.Abs(width - widths[index]) > .5).Any();
            var rowSnapshots = new List<(TableRowVisual, Rect[], Rect[])>();
            // Stacked rows and converged unchanged tables need no pointer walks. A reflow
            // gets a fresh geometry snapshot on the following layout pass.
            if (!stacked && !table.Stacked && (targetsChanged || (!table.AlignmentSettled && table.AlignmentPasses < 8)))
            {
                foreach (var row in table.Rows)
                {
                    var starts = new Rect[row.Cells.Count];
                    var ends = new Rect[row.Cells.Count];
                    for (int i = 0; i < row.Cells.Count; i++)
                    {
                        var cell = row.Cells[i];
                        starts[i] = cell.Content.ContentStart.GetCharacterRect(LogicalDirection.Forward);
                        ends[i] = cell.Content.ContentEnd.GetCharacterRect(LogicalDirection.Backward);
                    }
                    rowSnapshots.Add((row, starts, ends));
                }
            }
            snapshots.Add(new(table, available, widths, rowSnapshots, table.HasMultilineCells));
        }
        bool changed = false;
        foreach (var snapshot in snapshots)
        {
            var table = snapshot.Table;
            bool targetsChanged = Math.Abs(table.AvailableWidth - snapshot.Available) > 1
                || table.Widths.Length != snapshot.Widths.Length
                || table.Widths.Where((width, index) => Math.Abs(width - snapshot.Widths[index]) > .5).Any();
            if (targetsChanged)
            {
                table.AlignmentPasses = 0;
                table.AlignmentSettled = false;
            }
            table.AvailableWidth = snapshot.Available;
            table.Widths = snapshot.Widths;
            bool stacked = snapshot.Multiline || snapshot.Widths.Sum() + Math.Max(0, snapshot.Widths.Length - 1) * 24 > snapshot.Available;
            if (table.Stacked != stacked)
            {
                table.Stacked = stacked;
                table.AlignmentSettled = false;
                ReflowTable(table);
                changed = true;
                continue;
            }
            if (stacked || table.AlignmentSettled || table.AlignmentPasses >= 8) continue;
            table.AlignmentPasses++;
            bool spacingChanged = false;
            foreach (var (row, starts, ends) in snapshot.Rows)
            {
                if (starts.Length < 2) continue;
                bool wrapped = starts.Where((rect, index) => Math.Abs(rect.Y - starts[0].Y) > 8 || Math.Abs(rect.Y - ends[index].Y) > 8).Any();
                if (wrapped)
                {
                    foreach (var cell in row.Cells)
                        if (cell.Gap.CharacterSpacing != 0) { cell.Gap.CharacterSpacing = 0; spacingChanged = true; }
                    continue;
                }
                for (int i = 0; i + 1 < row.Cells.Count; i++)
                {
                    // Local deltas cancel shifts introduced by every earlier column.
                    double correction = table.Widths[i] + 24 - (starts[i + 1].X - starts[i].X);
                    if (Math.Abs(correction) < .6) continue;
                    var gap = row.Cells[i].Gap;
                    int spacing = gap.CharacterSpacing + (int)Math.Round(correction * 1000 / (14 * gap.Text.Length));
                    spacing = Math.Clamp(spacing, 0, 100000);
                    if (spacing != gap.CharacterSpacing) { gap.CharacterSpacing = spacing; spacingChanged = true; }
                }
            }
            table.AlignmentSettled = !spacingChanged;
            changed |= spacingChanged;
        }
        if (changed)
        {
            _tableLayoutDirty = true;
            _backgroundsDirty = true;
            _formulaBaselines.Clear();
            _document.InvalidateMeasure();
        }
    }

    private static void ReflowTable(TableVisual table)
    {
        foreach (var row in table.Rows)
        {
            row.Paragraph.Inlines.Clear();
            for (int i = 0; i < row.Cells.Count; i++)
            {
                var cell = row.Cells[i];
                cell.Gap.CharacterSpacing = 0;
                if (table.Stacked && !row.Header)
                {
                    if (i > 0) row.Paragraph.Inlines.Add(new LineBreak());
                    row.Paragraph.Inlines.Add(new Run { Text = table.Labels[i] + "：", FontWeight = FontWeights.SemiBold });
                }
                row.Paragraph.Inlines.Add(cell.Content);
                if (i + 1 < row.Cells.Count)
                {
                    if (!table.Stacked) row.Paragraph.Inlines.Add(cell.Gap);
                    else if (row.Header) row.Paragraph.Inlines.Add(new Run { Text = "   " });
                }
            }
        }
    }

    private void TrimTables()
    {
        foreach (var table in _tables.Where(table => table.Rows.Any(row => !_document.Blocks.Contains(row.Paragraph))).ToArray())
        {
            foreach (var row in table.Rows)
                foreach (var cell in row.Cells)
                    foreach (var run in TableCellRuns(cell.Content)) _tableRunCells.Remove(run);
            _tables.Remove(table);
        }
        _tableLayoutDirty = true;
    }

    private void AddTableBackgrounds()
    {
        foreach (var table in _tables)
        {
            var rowRects = table.Rows.Select(row => (First: row.Paragraph.ContentStart.GetCharacterRect(LogicalDirection.Forward),
                Last: row.Paragraph.ContentEnd.GetCharacterRect(LogicalDirection.Backward))).ToArray();
            if (rowRects.Length == 0) continue;
            double width = Math.Max(0, ActualWidth - table.Indent);
            var rect = new Rect(table.Indent, Math.Max(0, rowRects[0].First.Y - 6), width,
                Math.Max(rowRects[0].First.Height, rowRects[^1].Last.Bottom - rowRects[0].First.Y) + 12);
            int before = _backgrounds.Children.Count;
            AddBackground(rect, true);
            // Table surfaces go behind inline-code decorations already collected in this canvas.
            if (_backgrounds.Children.Count > before)
            {
                var background = _backgrounds.Children[^1];
                _backgrounds.Children.RemoveAt(_backgrounds.Children.Count - 1);
                _backgrounds.Children.Insert(0, background);
            }
            for (int i = 1; i < rowRects.Length; i++)
            {
                var rule = new Border { Width = width, Height = 1, Background = _tableRule };
                Canvas.SetLeft(rule, table.Indent);
                Canvas.SetTop(rule, (rowRects[i - 1].Last.Bottom + rowRects[i].First.Y) / 2);
                _backgrounds.Children.Add(rule);
            }
        }
    }
}
