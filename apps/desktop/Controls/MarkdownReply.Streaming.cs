using System.Text;
using Markdig;
using Markdig.Extensions.Mathematics;
using Markdig.Syntax;
using Markdig.Syntax.Inlines;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Documents;
using KYNXA_Desktop.Services;
using MdBlock = Markdig.Syntax.Block;

namespace KYNXA_Desktop.Controls;

public sealed partial class MarkdownReply
{
    private sealed record RenderedBlock(string Key, int Paragraphs, int Formulas, int Backgrounds);
    private readonly List<RenderedBlock> _renderedBlocks = [];
    private DispatcherTimer? _deferredRender;
    private bool _renderPending;
    private bool _renderedPlainText;
    private bool _renderedStreaming;

    public bool IsStreaming
    {
        get => (bool)GetValue(IsStreamingProperty);
        set => SetValue(IsStreamingProperty, value);
    }

    public static readonly DependencyProperty IsStreamingProperty = DependencyProperty.Register(
        nameof(IsStreaming), typeof(bool), typeof(MarkdownReply), new PropertyMetadata(false,
            (sender, _) => ((MarkdownReply)sender).Render()));

    private void Render()
    {
        // Updating a RichTextBlock invalidates its native TextPointers. This also applies
        // to final reconciliation and to selections spanning an earlier message.
        // RichTextBlock 更新会使原生 TextPointer 失效；最终内容校正和跨旧消息选择也适用。
        if (_selectionScroll.HasSelection)
        {
            _renderPending = true;
            if (_deferredRender is null)
            {
                _deferredRender = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(100) };
                _deferredRender.Tick += (_, _) => { if (!_selectionScroll.HasSelection) Render(); };
            }
            if (IsLoaded) _deferredRender.Start();
            return;
        }

        _renderPending = false;
        _deferredRender?.Stop();
        bool completed = _renderedStreaming && !IsStreaming;
        _renderedStreaming = IsStreaming;
        if (completed)
        {
            // Final text can equal the last delta. Still reconcile decoration geometry
            // after the final native layout without replacing selectable paragraphs.
            // 最终文本可能与最后一个增量相同；最终布局后仍校正装饰几何，同时保留可选择段落。
            _backgroundsDirty = true;
            _formulaBaselines.Clear();
            _document.InvalidateMeasure();
        }
        string source = IsPlainText ? Text ?? string.Empty : MathMarkdown.Normalize(Text ?? string.Empty);
        var blocks = IsPlainText ? [] : Markdown.Parse(source, Pipeline).ToArray();
        var keys = IsPlainText ? [$"plain:{FontSize}:{source}"] : blocks.Select(block => BlockKey(block, source)).ToArray();
        int keep = 0;
        if (_renderedPlainText == IsPlainText)
            while (keep < keys.Length && keep < _renderedBlocks.Count && _renderedBlocks[keep].Key == keys[keep]) keep++;
        if (keep == keys.Length && keep == _renderedBlocks.Count) { QueueFormulaRendering(); return; }

        // A growing list/fence/table can change its final block. Retain every preceding
        // paragraph and its formula images rather than rebuilding the whole message.
        // 增量可能改变列表、代码块或表格的末块；保留此前段落与公式图片，避免重建整条消息。
        RemoveTail(keep);
        _renderedPlainText = IsPlainText;
        _document.FontSize = IsPlainText ? FontSize : 14;
        _document.LineHeight = IsPlainText ? 26 : 23;
        for (int index = keep; index < keys.Length; index++)
        {
            int paragraphs = _document.Blocks.Count, formulas = _formulas.Count, backgrounds = _backgroundRanges.Count;
            if (IsPlainText) NewParagraph().Inlines.Add(new Run { Text = source });
            else AddBlock(blocks[index]);
            _renderedBlocks.Add(new RenderedBlock(keys[index], _document.Blocks.Count - paragraphs,
                _formulas.Count - formulas, _backgroundRanges.Count - backgrounds));
        }
        _backgroundsDirty = true;
        _tableLayoutDirty = true;
        _document.Select(_document.ContentStart, _document.ContentStart);
        QueueFormulaRendering();
    }

    private void RemoveTail(int keep)
    {
        int paragraphs = 0, formulas = 0, backgrounds = 0;
        foreach (var rendered in _renderedBlocks.Take(keep))
        {
            paragraphs += rendered.Paragraphs;
            formulas += rendered.Formulas;
            backgrounds += rendered.Backgrounds;
        }
        for (int index = _formulas.Count - 1; index >= formulas; index--)
        {
            _formulaBaselines.Remove(_formulas[index].Paragraph);
            _formulaLayer.Children.Remove(_formulas[index].Image);
            _formulas.RemoveAt(index);
        }
        _backgroundRanges.RemoveRange(backgrounds, _backgroundRanges.Count - backgrounds);
        while (_document.Blocks.Count > paragraphs) _document.Blocks.RemoveAt(_document.Blocks.Count - 1);
        TrimTables();
        _renderedBlocks.RemoveRange(keep, _renderedBlocks.Count - keep);
    }

    private string BlockKey(MdBlock block, string source)
    {
        int start = Math.Clamp(block.Span.Start, 0, source.Length);
        int end = Math.Clamp(block.Span.End + 1, start, source.Length);
        var key = new StringBuilder(block.GetType().Name).Append(':').Append(source, start, end - start);
        AddSemanticKey(key, block);
        return key.ToString();
    }

    private void AddSemanticKey(StringBuilder key, MdBlock block)
    {
        // A later reference definition can change an earlier link without changing its
        // source slice. Include resolved link targets and incomplete-math state.
        // 后续引用定义可能改变早期链接而不改变源文切片；缓存签名包含解析后的链接与未完成公式状态。
        if (block is MathBlock math)
            key.Append("|math:").Append(IsStreaming && math.ClosingFencedCharCount == 0);
        if (block is FencedCodeBlock fence && IsMathFence(fence)) key.Append("|math-fence:").Append(IsStreaming);
        if (block is LeafBlock { Inline: { } inline }) AddLinkKeys(key, inline);
        if (block is ContainerBlock container)
            foreach (var child in container) AddSemanticKey(key, child);
    }

    private static bool IsMathFence(FencedCodeBlock fence) => string.Equals(fence.Info?.Trim(), "math", StringComparison.OrdinalIgnoreCase);

    private static void AddLinkKeys(StringBuilder key, ContainerInline container)
    {
        foreach (var inline in container)
        {
            if (inline is LinkInline link) key.Append("|link:").Append(link.Url).Append(':').Append(link.IsImage);
            if (inline is ContainerInline nested) AddLinkKeys(key, nested);
        }
    }
}
