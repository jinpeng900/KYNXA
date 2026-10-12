using System.Globalization;
using System.Net;
using Markdig;
using Markdig.Extensions.Mathematics;
using Markdig.Extensions.Tables;
using Markdig.Renderers;
using Markdig.Renderers.Html;
using Markdig.Renderers.Html.Inlines;
using Markdig.Syntax;
using Markdig.Syntax.Inlines;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Renders selectable conversation HTML with deferred, source-preserving math and diagrams.
/// 把聊天 Markdown 渲染为可选择 HTML，公式和图表延迟渲染且保留源文。
/// </summary>
internal static class TranscriptMarkdown
{
    internal sealed record StreamingState(string Source, int StableLength, string StableHtml, int ParsedCharacters, int StableBlocks = 0);
    internal sealed record StreamingResult(string Html, StreamingState State);
    private const int MaximumStreamingTailCharacters = 32768;
    private static long _parseCount, _parsedCharacters;
    internal static long ParseCount => Interlocked.Read(ref _parseCount);
    internal static long ParsedCharacters => Interlocked.Read(ref _parsedCharacters);
    private static readonly MarkdownPipeline Pipeline = new MarkdownPipelineBuilder()
        .UsePipeTables().UseEmphasisExtras().UseTaskLists().UseAutoLinks().UseMathematics()
        // Preserve reply line breaks in both streaming and final HTML without rewriting source.
        // 流式与最终显示均保留回复换行，不改写原文或按编号猜测断行。
        .UseSoftlineBreakAsHardlineBreak()
        .DisableHtml().Build();

    public static string Render(string markdown, bool streaming = false)
    {
        if (string.IsNullOrEmpty(markdown)) return string.Empty;
        Interlocked.Increment(ref _parseCount); Interlocked.Add(ref _parsedCharacters, markdown.Length);
        var document = Markdown.Parse(MathMarkdown.Normalize(markdown), Pipeline);
        return RenderBlocks(document, streaming);
    }

    /// <summary>
    /// Completed top-level blocks are reused; the unfinished tail is parsed independently.
    /// Cross-block references remain provisional until the final authoritative full render.
    /// 复用已完成顶层块，仅独立解析活动尾部；跨块引用在结束时进行正式全量渲染校正。
    /// </summary>
    internal static StreamingResult RenderStreaming(string markdown, StreamingState? previous)
    {
        string source = MathMarkdown.Normalize(markdown);
        bool append = previous is not null && source.StartsWith(previous.Source, StringComparison.Ordinal);
        bool references = source.Contains("][", StringComparison.Ordinal) || source.Contains("]:", StringComparison.Ordinal);
        int stableLength = append && !references ? previous!.StableLength : 0;
        string stableHtml = append && !references ? previous!.StableHtml : string.Empty;
        int stableBlocks = append && !references ? previous!.StableBlocks : 0;
        string tail = source[stableLength..];
        // A single huge unfinished block must not repeatedly build a growing Markdown AST.
        // Its literal source remains selectable; final rendering restores all Markdown semantics.
        // 单个超长未完成块不能反复构建增长的 Markdown AST；先保留可选原文，结束后恢复完整语义。
        if (tail.Length > MaximumStreamingTailCharacters)
            return new(stableHtml + "<div class=\"streaming-raw\" style=\"white-space:pre-wrap\">" + WebUtility.HtmlEncode(tail) + "</div>",
                new(source, stableLength, stableHtml, 0, stableBlocks));
        var document = Markdown.Parse(tail, Pipeline);
        Interlocked.Increment(ref _parseCount); Interlocked.Add(ref _parsedCharacters, tail.Length);
        // Reference definitions may alter earlier blocks. Retain their common document context
        // and do not freeze prefixes containing reference syntax.
        // 引用定义可能改变前面的块；含引用语法时保留共同文档上下文，不冻结前缀。
        if (!references && document.Count > 1)
        {
            int lastStart = document[^1].Span.Start;
            if (lastStart > 0 && lastStart <= tail.Length)
            {
                stableHtml += RenderBlocks(document.Take(document.Count - 1), streaming: true);
                stableLength += lastStart;
                return new(stableHtml + RenderBlocks(document.TakeLast(1), streaming: true),
                    new(source, stableLength, stableHtml, tail.Length, stableBlocks + document.Count - 1));
            }
        }
        return new(stableHtml + RenderBlocks(document, streaming: true),
            new(source, stableLength, stableHtml, tail.Length, stableBlocks));
    }

    private static string RenderBlocks(IEnumerable<Block> blocks, bool streaming)
    {
        using var writer = new StringWriter(CultureInfo.InvariantCulture);
        var renderer = new HtmlRenderer(writer);
        Pipeline.Setup(renderer);
        renderer.ObjectRenderers.ReplaceOrAdd<LineBreakInlineRenderer>(new ReplyLineBreakRenderer());
        renderer.ObjectRenderers.ReplaceOrAdd<HtmlMathInlineRenderer>(new FormulaInlineRenderer());
        renderer.ObjectRenderers.ReplaceOrAdd<HtmlMathBlockRenderer>(new FormulaBlockRenderer(streaming));
        renderer.ObjectRenderers.ReplaceOrAdd<CodeBlockRenderer>(new HighlightedCodeRenderer(streaming));
        renderer.ObjectRenderers.ReplaceOrAdd<LinkInlineRenderer>(new SafeLinkRenderer());
        renderer.ObjectRenderers.ReplaceOrAdd<AutolinkInlineRenderer>(new SafeAutolinkRenderer());
        renderer.ObjectRenderers.ReplaceOrAdd<HtmlTableRenderer>(new ScrollTableRenderer());
        if (blocks is MarkdownDocument document) renderer.Render(document);
        else foreach (var block in blocks) renderer.Render(block);
        return writer.ToString();
    }

    private sealed class ReplyLineBreakRenderer : LineBreakInlineRenderer
    {
        protected override void Write(HtmlRenderer renderer, LineBreakInline line)
        {
            if (renderer.IsLastInContainer) return;
            if (renderer.EnableHtmlForInline && (line.IsHard || RenderAsHardlineBreak))
            {
                // A formatting LF after BR becomes an extra space in selected-text copying.
                // BR 后附加的 HTML 排版换行会在选区复制中变成额外空格，故只输出换行标签。
                renderer.Write("<br />");
                return;
            }
            renderer.EnsureLine();
        }
    }

    private static void WriteFormula(HtmlRenderer renderer, string latex, bool display, bool inline = false)
    {
        string tag = display && !inline ? "div" : "span";
        string delimiter = display ? "$$" : "$";
        renderer.Write("<").Write(tag).Write(" class=\"math\" data-latex=\"")
            .Write(WebUtility.HtmlEncode(latex)).Write("\" data-display=\"")
            .Write(display ? "true" : "false").Write("\">")
            .WriteEscape(delimiter + latex + delimiter).Write("</").Write(tag).Write(">");
        if (display && !inline) renderer.EnsureLine();
    }

    private sealed class FormulaInlineRenderer : HtmlObjectRenderer<MathInline>
    {
        protected override void Write(HtmlRenderer renderer, MathInline formula) =>
            WriteFormula(renderer, formula.Content.ToString(), display: formula.DelimiterCount > 1, inline: true);
    }

    private sealed class FormulaBlockRenderer(bool streaming) : HtmlObjectRenderer<MathBlock>
    {
        protected override void Write(HtmlRenderer renderer, MathBlock formula)
        {
            renderer.EnsureLine();
            string latex = formula.Lines.ToString();
            if (streaming && formula.ClosingFencedCharCount == 0)
            {
                renderer.Write("<pre class=\"math-source\">").WriteEscape("$$\n" + latex).WriteLine("</pre>");
                return;
            }
            WriteFormula(renderer, latex, display: true);
        }
    }

    private sealed class HighlightedCodeRenderer(bool streaming) : HtmlObjectRenderer<CodeBlock>
    {
        protected override void Write(HtmlRenderer renderer, CodeBlock code)
        {
            renderer.EnsureLine();
            string source = code.Lines.ToString();
            string language = code is FencedCodeBlock fence
                ? (fence.Info ?? string.Empty).Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).FirstOrDefault() ?? string.Empty
                : string.Empty;
            if (code is FencedCodeBlock mermaidFence && string.Equals(language, "mermaid", StringComparison.OrdinalIgnoreCase))
            {
                // A cancelled or truncated final reply can still have an open fence; only a real closer permits rendering.
                // 取消或截断的最终回复也可能没有闭合围栏；只有真正闭合后才允许渲染，源码仅作为转义文本传递。
                bool isComplete = mermaidFence.ClosingFencedCharCount >= mermaidFence.OpeningFencedCharCount;
                renderer.Write("<div class=\"mermaid-block\" data-mermaid-ready=\"")
                    .Write(isComplete ? "true" : "false").Write("\"><pre class=\"mermaid-source\">")
                    .Write("<code class=\"language-mermaid\" data-language=\"mermaid\">")
                    .WriteEscape(source).WriteLine("</code></pre></div>");
                return;
            }
            if (!streaming && string.Equals(language, "math", StringComparison.OrdinalIgnoreCase))
            {
                WriteFormula(renderer, source, display: true);
                return;
            }
            // Earlier closed blocks are complete even while later prose is streaming.
            // 前面的框已闭合时应视为完整；后面的正文仍流式生成不影响它的复制提示。
            bool codeComplete = code is FencedCodeBlock codeFence
                ? codeFence.ClosingFencedCharCount >= codeFence.OpeningFencedCharCount
                : !streaming;
            renderer.Write("<pre data-code-complete=\"").Write(codeComplete ? "true" : "false").Write("\"><code");
            if (language.Length > 0)
                renderer.Write(" class=\"language-").Write(WebUtility.HtmlEncode(language))
                    .Write("\" data-language=\"").Write(WebUtility.HtmlEncode(language)).Write("\"");
            renderer.Write(">");
            foreach (var token in CodeSyntaxHighlighter.Highlight(source, language))
            {
                if (token.Color is uint color)
                    renderer.Write("<span style=\"color:#").Write((color & 0xFFFFFF).ToString("x6", CultureInfo.InvariantCulture)).Write("\">");
                renderer.WriteEscape(token.Text);
                if (token.Color.HasValue) renderer.Write("</span>");
            }
            renderer.WriteLine("</code></pre>");
        }
    }

    private sealed class ScrollTableRenderer : HtmlTableRenderer
    {
        protected override void Write(HtmlRenderer renderer, Table table)
        {
            renderer.EnsureLine();
            renderer.WriteLine("<div class=\"table-scroll\" tabindex=\"0\">");
            base.Write(renderer, table);
            renderer.WriteLine("</div>");
        }
    }

    private static bool IsSafeUrl(string? value)
    {
        if (string.IsNullOrWhiteSpace(value) || value.Any(char.IsControl)
            || !Uri.TryCreate(value, UriKind.Absolute, out var uri)) return false;
        return uri.Scheme is "http" or "https" ? uri.Host.Length > 0
            : uri.Scheme == "mailto" && uri.AbsolutePath.Length > 0;
    }

    private static void OpenLink(HtmlRenderer renderer, string url, string? title, bool image = false)
    {
        renderer.Write("<a href=\"").Write(WebUtility.HtmlEncode(url)).Write("\" rel=\"noopener noreferrer\"");
        if (image) renderer.Write(" class=\"image-link\"");
        if (!string.IsNullOrEmpty(title)) renderer.Write(" title=\"").Write(WebUtility.HtmlEncode(title)).Write("\"");
        renderer.Write(">");
    }

    private sealed class SafeLinkRenderer : HtmlObjectRenderer<LinkInline>
    {
        protected override void Write(HtmlRenderer renderer, LinkInline link)
        {
            string? url = link.GetDynamicUrl?.Invoke() ?? link.Url;
            bool nested = false;
            for (var parent = link.Parent; parent is not null; parent = parent.Parent)
                if (parent is LinkInline { IsImage: false }) { nested = true; break; }
            bool linked = !nested && IsSafeUrl(url);
            if (linked) OpenLink(renderer, url!, link.Title, link.IsImage);
            if (link.FirstChild is null && link.IsImage) renderer.WriteEscape(url ?? string.Empty);
            else renderer.WriteChildren(link);
            if (linked) renderer.Write("</a>");
        }
    }

    private sealed class SafeAutolinkRenderer : HtmlObjectRenderer<AutolinkInline>
    {
        protected override void Write(HtmlRenderer renderer, AutolinkInline link)
        {
            string url = link.IsEmail ? "mailto:" + link.Url : link.Url;
            bool linked = IsSafeUrl(url);
            if (linked) OpenLink(renderer, url, title: null);
            renderer.WriteEscape(link.Url);
            if (linked) renderer.Write("</a>");
        }
    }
}
