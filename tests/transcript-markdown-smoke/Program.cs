using System.Net;
using System.Text.RegularExpressions;
using KYNXA_Desktop.Services;

int checks = 0;
void Check(bool condition, string description)
{
    if (!condition) throw new InvalidOperationException(description);
    checks++;
}
string Text(string html) => WebUtility.HtmlDecode(Regex.Replace(html, "<[^>]+>", string.Empty));

string table = TranscriptMarkdown.Render("""
Before.

| Label | Formula | Count |
| :--- | :---: | ---: |
| **fraction** | $\frac{1}{2}$ | `2` |
| alternate | \(\theta\) | 3 |
| empty | | 0 |

After.
""");
Check(table.Contains("class=\"table-scroll\"") && table.Contains("<table>") && table.Contains("<thead>"), "Semantic table has a scroll wrapper and header.");
Check(table.Contains("<strong>fraction</strong>") && table.Contains("<code>2</code>"), "Table cells retain rich inline content.");
Check(Regex.Matches(table, "class=\"math\"").Count == 2 && table.Contains("data-latex=\"\\frac{1}{2}\""), "Both table formulas retain TeX.");
Check(table.Contains("text-align: center") && table.Contains("text-align: right"), "Column alignment is retained.");
Check(table.StartsWith("<p>Before.</p>") && table.TrimEnd().EndsWith("<p>After.</p>"), "Output contains top-level blocks without a document wrapper.");

string formula = TranscriptMarkdown.Render("$$\nx < y & \\text{\"quoted\"}\n$$");
Check(formula.Contains("data-display=\"true\"") && formula.Contains("&quot;quoted&quot;"), "Display formula attributes are escaped.");
Check(formula.Contains("x &lt; y &amp;"), "Formula fallback cannot create markup.");
Check(TranscriptMarkdown.Render("$$\n\\frac{1}{", true).Contains("class=\"math-source\""), "Incomplete streaming display formula stays source.");
Check(!TranscriptMarkdown.Render("$\\frac{1}{", true).Contains("class=\"math\""), "Incomplete inline formula stays source.");
string inlineDisplay = TranscriptMarkdown.Render("Before $$x^2$$ after.");
Check(inlineDisplay.Contains("<span class=\"math\"") && inlineDisplay.Contains("data-display=\"true\"")
    && inlineDisplay.Contains("$$x^2$$") && !inlineDisplay.Contains("<div"), "Inline double-dollar source and display mode survive without invalid nested block HTML.");

string fence = "```math\n\\begin{matrix}a&b\\\\c&d\\end{matrix}\n```";
Check(Regex.IsMatch(TranscriptMarkdown.Render(fence, true), "<pre\\b[^>]*><code\\b"), "Streaming math fence remains code, including completion metadata.");
Check(TranscriptMarkdown.Render(fence).Contains("class=\"math\""), "Settled math fence becomes display math.");
Check(!TranscriptMarkdown.Render("```tex\n$\\theta$\n```").Contains("class=\"math\""), "Ordinary TeX code is not interpreted as math.");

string source = "const text = \"<script>alert(1)</script>\";\nconst answer = 42;";
string code = TranscriptMarkdown.Render("```js\n" + source + "\n```");
Check(code.Contains("<span style=\"color:#"), "Supported code receives token colors.");
Check(Text(code).TrimEnd('\r', '\n') == source, "Highlighting preserves the entire copied source.");
Check(!code.Contains("<script>"), "Code is escaped.");

string unsafeHtml = TranscriptMarkdown.Render("""
<script>alert('x')</script>

[bad](javascript:alert%281%29) [data](data:text/html,hello) [local](file:///C:/secret) [relative](/settings)

[safe](https://example.com/?a=1&b=2 "a & b") <mailto:hello@example.com>

![picture](https://example.com/image.png) ![bad image](javascript:alert%281%29)

[![badge](https://example.com/badge.png)](https://example.com/outer)
""");
Check(!Regex.IsMatch(unsafeHtml, "<(script|img|iframe|object|style)\\b", RegexOptions.IgnoreCase), "Raw HTML and images cannot create executable elements or fetch images.");
Check(!Regex.IsMatch(unsafeHtml, "href=\"(?:javascript|data|file|/)"), "Unsupported destinations are not links.");
Check(unsafeHtml.Contains("href=\"https://example.com/?a=1&amp;b=2\"") && unsafeHtml.Contains("mailto:hello@example.com"), "Safe web and email links remain available.");
Check(unsafeHtml.Contains("class=\"image-link\"") && unsafeHtml.Contains(">picture</a>"), "Image Markdown becomes a text link.");
Check(!Regex.IsMatch(unsafeHtml, "<a[^>]*>\\s*<a"), "Linked images do not create nested anchors.");
Check(Text(unsafeHtml).Contains("bad image") && Text(unsafeHtml).Contains("<script>alert('x')</script>"), "Unsafe content remains readable text.");

var longText = string.Join("\n\n", Enumerable.Range(0, 300).Select(index => $"段落{index} **完整语义** $x_{index}$。"));
var firstStream = TranscriptMarkdown.RenderStreaming(longText + "\n\n活动尾部", null);
Check(firstStream.State.StableLength > longText.Length - 100, "Complete top-level blocks become a reusable stable prefix.");
var nextStream = TranscriptMarkdown.RenderStreaming(longText + "\n\n活动尾部继续", firstStream.State);
Check(nextStream.State.ParsedCharacters < 64 && nextStream.Html.Contains("活动尾部继续"), "Appending to a long reply parses only its active tail.");
Check(nextStream.Html.StartsWith(firstStream.State.StableHtml, StringComparison.Ordinal), "Stable rendered blocks retain their original HTML.");
var changedStream = TranscriptMarkdown.RenderStreaming("纠正了前文\n\n新的尾部", nextStream.State);
Check(!changedStream.Html.Contains("段落299") && changedStream.Html.Contains("纠正了前文"), "A rewritten prefix discards stale streaming state.");
var referenceStream = TranscriptMarkdown.RenderStreaming("[label]\n\n后文", null);
referenceStream = TranscriptMarkdown.RenderStreaming("[label]\n\n后文\n\n[label]: https://example.com", referenceStream.State);
Check(referenceStream.State.StableLength == 0 && referenceStream.Html.Contains("href=\"https://example.com\""), "A late reference definition reparses its document context.");
var rawTail = TranscriptMarkdown.RenderStreaming(new string('长', 40000) + "<script>literal</script>", null);
Check(rawTail.State.ParsedCharacters == 0 && !rawTail.Html.Contains("<script>"), "An oversized unfinished block remains escaped selectable text without a growing AST.");
Check(Text(rawTail.Html) == new string('长', 40000) + "<script>literal</script>", "Deferred long-block rendering preserves every original character.");
Check(TranscriptMarkdown.Render(longText + "\n\n活动尾部继续").Contains("<strong>完整语义</strong>"), "Final full rendering retains authoritative Markdown semantics.");
Console.WriteLine($"Transcript Markdown smoke: {checks} checks passed.");
