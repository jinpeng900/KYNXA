using Markdig;
using Markdig.Extensions.Tables;
using Markdig.Extensions.TaskLists;
using Markdig.Syntax;
using Markdig.Syntax.Inlines;
using Microsoft.UI;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Media;
using Windows.UI.Text;
using KYNXA_Desktop.Services;
using MdBlock = Markdig.Syntax.Block;
using MdInline = Markdig.Syntax.Inlines.Inline;
using Paragraph = Microsoft.UI.Xaml.Documents.Paragraph;
using Span = Microsoft.UI.Xaml.Documents.Span;

namespace KYNXA_Desktop.Controls;

/// <summary>One selectable document, including across paragraphs and code fences.</summary>
public sealed partial class MarkdownReply : UserControl
{
    private static readonly MarkdownPipeline Pipeline = new MarkdownPipelineBuilder()
        .UsePipeTables().UseEmphasisExtras().UseTaskLists().UseAutoLinks().DisableHtml().Build();
    private static readonly FontFamily CodeFont = new("Cascadia Mono, Consolas, Microsoft YaHei UI");
    private readonly RichTextBlock _document = new()
    {
        FontFamily = new FontFamily("Microsoft YaHei UI"), FontSize = 14,
        LineHeight = 23, LineStackingStrategy = LineStackingStrategy.MaxHeight,
        IsTextSelectionEnabled = true, TextWrapping = TextWrapping.Wrap,
        HorizontalAlignment = HorizontalAlignment.Stretch,
    };
    private readonly TextSelectionAutoScroll _selectionScroll;
    private readonly Dictionary<uint, SolidColorBrush> _codeBrushes = [];

    public MarkdownReply()
    {
        var surface = new Grid();
        surface.Children.Add(_backgrounds);
        surface.Children.Add(_document);
        Content = surface;
        _selectionScroll = new TextSelectionAutoScroll(_document);
        _document.SizeChanged += (_, _) => _backgroundsDirty = true;
        _document.LayoutUpdated += (_, _) => UpdateBackgrounds();
    }

    internal RichTextBlock Document => _document;
    internal string SelectedConversationText => _selectionScroll.SelectedConversationText;

    public bool IsPlainText
    {
        get => (bool)GetValue(IsPlainTextProperty);
        set => SetValue(IsPlainTextProperty, value);
    }
    public static readonly DependencyProperty IsPlainTextProperty = DependencyProperty.Register(
        nameof(IsPlainText), typeof(bool), typeof(MarkdownReply), new PropertyMetadata(false,
            (sender, _) => ((MarkdownReply)sender).Render()));

    public string Text
    {
        get => (string)GetValue(TextProperty);
        set => SetValue(TextProperty, value);
    }
    public static readonly DependencyProperty TextProperty = DependencyProperty.Register(
        nameof(Text), typeof(string), typeof(MarkdownReply),
        new PropertyMetadata(string.Empty, (sender, _) => ((MarkdownReply)sender).Render()));

    private void Render()
    {
        _selectionScroll.Stop();
        _document.Blocks.Clear();
        _backgroundRanges.Clear();
        _backgrounds.Children.Clear();
        _backgroundsDirty = true;
        _document.FontSize = IsPlainText ? FontSize : 14;
        _document.LineHeight = IsPlainText ? 26 : 23;
        if (IsPlainText) NewParagraph().Inlines.Add(new Run { Text = Text ?? string.Empty });
        else foreach (var block in Markdown.Parse(Text ?? string.Empty, Pipeline)) AddBlock(block);
        _document.Select(_document.ContentStart, _document.ContentStart);
    }

    private Paragraph NewParagraph(double indent = 0, string prefix = "")
    {
        var paragraph = new Paragraph { Margin = new Thickness(indent, _document.Blocks.Count == 0 ? 0 : 10, 0, 0) };
        if (prefix.Length > 0) paragraph.Inlines.Add(new Run { Text = prefix });
        _document.Blocks.Add(paragraph);
        return paragraph;
    }

    private void AddBlock(MdBlock block, double indent = 0, string prefix = "", bool quote = false)
    {
        switch (block)
        {
            case HeadingBlock heading:
                var title = NewParagraph(indent, prefix);
                title.FontSize = heading.Level switch { 1 => 23, 2 => 20, 3 => 17, _ => 15 };
                title.FontWeight = FontWeights.SemiBold;
                title.Margin = new Thickness(indent, _document.Blocks.Count == 1 ? 0 : 18, 0, 3);
                AddInlines(title.Inlines, heading.Inline);
                break;
            case CodeBlock code:
                var codeParagraph = NewParagraph(indent + 12, prefix);
                codeParagraph.FontFamily = CodeFont;
                codeParagraph.FontSize = 13;
                codeParagraph.LineHeight = 21;
                codeParagraph.Margin = new Thickness(indent + 12, 12, 12, 10);
                _backgroundRanges.Add((codeParagraph, true));
                foreach (var token in CodeSyntaxHighlighter.Highlight(code.Lines.ToString(), (code as FencedCodeBlock)?.Info))
                {
                    var run = new Run { Text = token.Text };
                    if (token.Color is { } color)
                    {
                        if (!_codeBrushes.TryGetValue(color, out var brush))
                            _codeBrushes[color] = brush = new SolidColorBrush(Windows.UI.Color.FromArgb(
                                (byte)(color >> 24), (byte)(color >> 16), (byte)(color >> 8), (byte)color));
                        run.Foreground = brush;
                    }
                    codeParagraph.Inlines.Add(run);
                }
                break;
            case ListBlock list:
                int number = int.TryParse(list.OrderedStart, out int start) ? start : 1;
                foreach (ListItemBlock item in list)
                {
                    bool first = true;
                    foreach (var child in item)
                    {
                        AddBlock(child, indent + 16, first ? (list.IsOrdered ? $"{number}.  " : "•  ") : "", quote);
                        first = false;
                    }
                    number++;
                }
                break;
            case QuoteBlock quoted:
                foreach (var child in quoted) AddBlock(child, indent + 14, "│  ", true);
                break;
            case ThematicBreakBlock:
                NewParagraph(indent).Inlines.Add(new Run { Text = "────────────────────", Foreground = new SolidColorBrush(Colors.LightGray) });
                break;
            case Table table:
                AddTable(table, indent);
                break;
            case LeafBlock leaf:
                var paragraph = NewParagraph(indent, prefix);
                if (quote) paragraph.Foreground = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 106, 106, 106));
                if (quote) _backgroundRanges.Add((paragraph, true));
                if (leaf.Inline is not null) AddInlines(paragraph.Inlines, leaf.Inline);
                else paragraph.Inlines.Add(new Run { Text = leaf.Lines.ToString() });
                break;
            case ContainerBlock container:
                foreach (var child in container) AddBlock(child, indent, prefix, quote);
                break;
        }
    }

    private void AddInlines(InlineCollection target, ContainerInline? container)
    {
        if (container is null) return;
        foreach (var inline in container) AddInline(target, inline);
    }

    private void AddInline(InlineCollection target, MdInline inline)
    {
        switch (inline)
        {
            case LiteralInline literal: target.Add(new Run { Text = literal.Content.ToString() }); break;
            case HtmlEntityInline entity: target.Add(new Run { Text = entity.Transcoded.ToString() }); break;
            case CodeInline code:
                var codeRun = new Run { Text = code.Content, FontFamily = CodeFont, FontSize = 13 };
                target.Add(codeRun);
                _backgroundRanges.Add((codeRun, false));
                break;
            case LineBreakInline: target.Add(new LineBreak()); break;
            case EmphasisInline emphasis:
                var span = new Span();
                if (emphasis.DelimiterChar == '~') span.TextDecorations = TextDecorations.Strikethrough;
                else if (emphasis.DelimiterCount >= 2) span.FontWeight = FontWeights.SemiBold;
                else span.FontStyle = FontStyle.Italic;
                AddInlines(span.Inlines, emphasis);
                target.Add(span);
                break;
            case LinkInline link:
                // Rendering makes no network requests; image references remain readable links.
                if (TryLink(link.Url, out var uri))
                {
                    var hyperlink = new Hyperlink { NavigateUri = uri };
                    if (link.IsImage) hyperlink.Inlines.Add(new Run { Text = "图片：" });
                    AddInlines(hyperlink.Inlines, link);
                    target.Add(hyperlink);
                }
                else AddInlines(target, link);
                break;
            case AutolinkInline auto:
                if (TryLink(auto.IsEmail ? "mailto:" + auto.Url : auto.Url, out var autoUri))
                {
                    var hyperlink = new Hyperlink { NavigateUri = autoUri };
                    hyperlink.Inlines.Add(new Run { Text = auto.Url });
                    target.Add(hyperlink);
                }
                else target.Add(new Run { Text = auto.Url });
                break;
            case TaskList task: target.Add(new Run { Text = task.Checked ? "☑ " : "☐ " }); break;
            case ContainerInline nested: AddInlines(target, nested); break;
        }
    }

    private static bool TryLink(string? url, out Uri? uri) =>
        Uri.TryCreate(url, UriKind.Absolute, out uri) && uri.Scheme is "https" or "http" or "mailto";

    private void AddTable(Table table, double indent)
    {
        // Keep tables in the same text-selection surface as the rest of the reply.
        var rows = table.Cast<TableRow>().Select(row => row.Cast<TableCell>()
            .Select(cell => string.Join(" ", cell.OfType<LeafBlock>().Select(leaf => PlainText(leaf.Inline))))
            .ToArray()).ToArray();
        if (rows.Length == 0) return;
        int columns = rows.Max(row => row.Length);
        var widths = Enumerable.Range(0, columns).Select(i => rows.Max(row => i < row.Length ? DisplayWidth(row[i]) : 0)).ToArray();
        var paragraph = NewParagraph(indent);
        paragraph.FontFamily = CodeFont;
        paragraph.FontSize = 13;
        _backgroundRanges.Add((paragraph, true));
        for (int r = 0; r < rows.Length; r++)
        {
            if (r > 0) paragraph.Inlines.Add(new LineBreak());
            string line = string.Join("  │  ", rows[r].Select((cell, i) => cell + new string(' ', Math.Max(0, widths[i] - DisplayWidth(cell)))));
            paragraph.Inlines.Add(new Run { Text = line, FontWeight = ((TableRow)table[r]).IsHeader ? FontWeights.SemiBold : FontWeights.Normal });
        }
    }

    private static string PlainText(ContainerInline? container)
    {
        if (container is null) return string.Empty;
        return string.Concat(container.Select(inline => inline switch
        {
            LiteralInline literal => literal.Content.ToString(), CodeInline code => code.Content,
            HtmlEntityInline entity => entity.Transcoded.ToString(), AutolinkInline auto => auto.Url,
            LineBreakInline => " ", ContainerInline nested => PlainText(nested), _ => string.Empty,
        }));
    }
    private static int DisplayWidth(string text) => text.EnumerateRunes().Sum(rune => rune.Value >= 0x2E80 ? 2 : 1);
}
