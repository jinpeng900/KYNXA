using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

internal sealed record TranscriptSegmentHtml(AssistantSegment Source, string Html, string ReasoningHtml,
    TranscriptMarkdown.StreamingState? Stream = null)
{
    public long Size => (long)Source.Content.Length + Source.Reasoning.Length + Html.Length + ReasoningHtml.Length
        + (Stream is null ? 0 : Stream.Source.Length + Stream.StableHtml.Length);
}

internal sealed record TranscriptHtml(string Role, string Content, string Reasoning, bool Streaming, string Mode,
    string Html, string ReasoningHtml, TranscriptSegmentHtml[] Segments, TranscriptMarkdown.StreamingState? Stream = null)
{
    public long Size => (long)Content.Length + Reasoning.Length + Html.Length + ReasoningHtml.Length
        + Segments.Sum(segment => segment.Size)
        + (Stream is null ? 0 : Stream.Source.Length + Stream.StableHtml.Length);
}

/// <summary>Pure formatting cache; message state and browser ownership remain with the view.
/// 纯排版缓存；消息状态和浏览器生命周期仍由视图持有。</summary>
internal static class TranscriptHtmlCache
{
    internal static TranscriptHtml Preview(string role, string content, string reasoning, bool streaming, string mode,
        AssistantSegment[] sources, TranscriptHtml? previous)
    {
        static string PlainHtml(string source) => source.Length == 0 ? ""
            : "<div class=\"markdown-pending\" style=\"white-space:pre-wrap\">"
                + System.Net.WebUtility.HtmlEncode(source.Length > 8192 ? source[^8192..] : source) + "</div>";
        var segments = sources.Select(segment => previous?.Segments.FirstOrDefault(value => value.Source == segment)
            ?? new TranscriptSegmentHtml(segment, PlainHtml(segment.Content), "")).ToArray();
        return new(role, content, reasoning, streaming, mode,
            role != "user" && segments.Length == 0 ? PlainHtml(content) : "", "", segments);
    }

    internal static TranscriptHtml Render(string role, string content, string reasoning, bool streaming, string mode,
        AssistantSegment[] sources, TranscriptHtml? previous)
    {
        var byId = previous?.Segments.ToDictionary(segment => segment.Source.Id, StringComparer.Ordinal);
        var segments = sources.Select(segment =>
        {
            TranscriptSegmentHtml? prior = null;
            byId?.TryGetValue(segment.Id, out prior);
            // State changes reuse formatting, while stream closure still requires a full Markdown render.
            // 状态变化复用排版；流结束仍须完整解析，校正引用、公式和未闭合代码块。
            if (prior is not null && prior.Source.Content == segment.Content
                && (prior.Source.Status == "streaming") == (segment.Status == "streaming"))
                return prior with { Source = segment };
            if (segment.Status == "streaming")
            {
                var result = TranscriptMarkdown.RenderStreaming(segment.Content, prior?.Stream);
                return new TranscriptSegmentHtml(segment, result.Html, "", result.State);
            }
            return new TranscriptSegmentHtml(segment, TranscriptMarkdown.Render(segment.Content), "");
        }).ToArray();
        string html = "";
        TranscriptMarkdown.StreamingState? state = null;
        if (role != "user" && segments.Length == 0)
        {
            if (previous is not null && previous.Content == content && previous.Streaming == streaming)
            { html = previous.Html; state = previous.Stream; }
            else if (streaming)
            {
                var result = TranscriptMarkdown.RenderStreaming(content, previous?.Stream);
                html = result.Html; state = result.State;
            }
            else html = TranscriptMarkdown.Render(content);
        }
        return new(role, content, reasoning, streaming, mode, html, "", segments, state);
    }
}
