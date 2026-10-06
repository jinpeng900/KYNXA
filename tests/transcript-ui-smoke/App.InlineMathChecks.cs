using System.Text.Json;

namespace TranscriptUiSmoke;

public partial class App
{
    private async Task CheckLongInlineMathAsync()
    {
        var originalSize = _window!.AppWindow.Size;
        string longTex = string.Join("+", Enumerable.Range(1, 60).Select(index => "a_{" + index + "}")) + @"+\mathrm{INLINEEND}";
        string markdown = "SHORT_LEFT $x^2$ SHORT_RIGHT\n\nLONG_INLINE_BEFORE $" + longTex + "$ LONG_INLINE_AFTER";
        var chat = Guid.NewGuid();
        _transcript.ClearSelection();
        try
        {
            _transcript.ShowConversation(chat, [Message(chat, "assistant", markdown)]);
            await WaitAsync("document.querySelectorAll('.message-body .math').length === 2 && document.querySelectorAll('.message-body .katex').length === 2 && document.fonts.status === 'loaded'",
                "short and deliberately oversized inline formulas render as real KaTeX DOM");
            double scale = _transcript.XamlRoot.RasterizationScale;
            foreach (int width in new[] { 480, 980 })
            {
                _window.AppWindow.Resize(new Windows.Graphics.SizeInt32((int)Math.Ceiling(width * scale), (int)Math.Ceiling(780 * scale)));
                await Task.Delay(220);
                await EvalAsync<bool>("(() => { scrollTo(0, 0); return true; })()");
                var geometry = await EvalAsync<JsonElement>("""
                    (() => {
                      const short = document.querySelectorAll('.message-body .math')[0];
                      const long = document.querySelectorAll('.message-body .math')[1];
                      const shortParagraph = short.closest('p');
                      const before = document.createRange();
                      before.selectNodeContents(shortParagraph.firstChild);
                      const after = document.createRange();
                      after.selectNodeContents(shortParagraph.lastChild);
                      const left = before.getBoundingClientRect(), right = after.getBoundingClientRect();
                      const small = short.getBoundingClientRect(), large = long.getBoundingClientRect();
                      const body = long.closest('.message-body').getBoundingClientRect();
                      return {
                        viewport: innerWidth, pageWidth: document.documentElement.scrollWidth,
                        shortInline: small.left >= left.right - 1 && right.left >= small.right - 1
                          && small.top <= left.bottom && small.bottom >= left.top
                          && small.top <= right.bottom && small.bottom >= right.top,
                        shortScrollWidth: short.scrollWidth, shortClientWidth: short.clientWidth,
                        longScrollWidth: long.scrollWidth, longClientWidth: long.clientWidth,
                        longLeft: large.left, longRight: large.right,
                        bodyLeft: body.left, bodyRight: body.right
                      };
                    })()
                    """);
                _metrics["inlineMath" + width] = geometry;
                Check(geometry.GetProperty("shortInline").GetBoolean() &&
                    geometry.GetProperty("shortScrollWidth").GetDouble() <= geometry.GetProperty("shortClientWidth").GetDouble() + 1,
                    "short inline math remains between the original text on the same line without an unnecessary scrollbar at " + width);
                Check(geometry.GetProperty("longScrollWidth").GetDouble() > geometry.GetProperty("longClientWidth").GetDouble() + 100 &&
                    geometry.GetProperty("longLeft").GetDouble() >= geometry.GetProperty("bodyLeft").GetDouble() - 1 &&
                    geometry.GetProperty("longRight").GetDouble() <= geometry.GetProperty("bodyRight").GetDouble() + 1 &&
                    geometry.GetProperty("pageWidth").GetDouble() <= geometry.GetProperty("viewport").GetDouble() + 1,
                    "oversized inline math scrolls locally inside the available reply width without clipping or widening the page at " + width);
                Check(await EvalAsync<bool>("""
                    (() => {
                      const math = document.querySelectorAll('.message-body .math')[1];
                      math.scrollLeft = math.scrollWidth;
                      const end = [...math.querySelectorAll('.katex-html .mord')].filter(node => node.textContent === 'INLINEEND').at(-1);
                      if (!end) return false;
                      const viewport = math.getBoundingClientRect(), tail = end.getBoundingClientRect();
                      return math.scrollLeft > 100 && tail.left >= viewport.left - 1 && tail.right <= viewport.right + 1;
                    })()
                    """), "scrolling reaches the actual final KaTeX term instead of leaving the end cropped at " + width);
                string copied = await EvalAsync<string>("""
                    (() => {
                      const math = document.querySelectorAll('.message-body .math')[1];
                      const range = document.createRange();
                      range.selectNodeContents(math);
                      const selection = getSelection();
                      selection.removeAllRanges(); selection.addRange(range);
                      return window.transcriptSelectionText();
                    })()
                    """);
                Check(copied == "$" + longTex + "$",
                    "selecting the complete scrollable formula copies its exact original TeX with delimiters at " + width);
                await EvalAsync<bool>("(() => { getSelection().removeAllRanges(); return true; })()");
                await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), "kynxa-transcript-inline-math-" + width + ".png"));
            }
        }
        finally
        {
            _transcript.ClearSelection();
            _window.AppWindow.Resize(originalSize);
            await Task.Delay(150);
        }
    }
}
