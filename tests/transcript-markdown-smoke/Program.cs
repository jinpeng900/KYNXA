using System.Net;
using System.Text.RegularExpressions;
using KYNXA_Desktop.Services;

int checks = 0;
void Check(bool condition, string description)
{
    if (!condition) throw new InvalidOperationException(description);
    checks++;
}
string Text(string html) => WebUtility.HtmlDecode(Regex.Replace(Regex.Replace(html, "<br\\s*/?>", "\n"), "<[^>]+>", string.Empty));

// Chat replies preserve actual source line breaks without guessing numbered boundaries.
// 聊天回复保留原文的真实换行，不通过猜测编号来拆分正文。
string numberedLines = string.Join("\n", Enumerable.Range(1, 400).Select(index => $"{index:000} 软件工程测试检查项"));
string multiline = TranscriptMarkdown.Render(numberedLines);
Check(Regex.Matches(multiline, "<br\\s*/?>").Count == 399, "Numbered reply retains each of its 400 source lines.");
Check(Text(multiline).TrimEnd('\r', '\n') == numberedLines, "Line-break presentation preserves the numbered source text.");
Check(Regex.Matches(TranscriptMarkdown.Render(numberedLines.Replace("\n", "\r\n")), "<br\\s*/?>").Count == 399,
    "Windows CRLF replies retain the same visible line breaks.");
Check(Regex.Matches(TranscriptMarkdown.Render("001 第一项\n002 第二项\n003 第", true), "<br\\s*/?>").Count == 2,
    "Streaming partial lines use the same line-break presentation as completed replies.");
Check(!TranscriptMarkdown.Render("001 第一项 002 第二项").Contains("<br"), "Single-line replies are not split by guessed number patterns.");
Check(Regex.Matches(TranscriptMarkdown.Render("第一段\n续行\n\n第二段"), "<p>").Count == 2,
    "Blank lines still delimit Markdown paragraphs.");
Check(Regex.Matches(TranscriptMarkdown.Render("第一行  \n第二行\\\n第三行"), "<br\\s*/?>").Count == 2,
    "Explicit Markdown hard breaks are not duplicated.");
string wrappedList = TranscriptMarkdown.Render("- 第一项\n  继续解释\n- 第二项");
Check(Regex.Matches(wrappedList, "<li>").Count == 2 && Regex.Matches(wrappedList, "<br\\s*/?>").Count == 1,
    "Soft breaks within a list item retain list structure.");

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
Check(TranscriptMarkdown.Render(fence, true).Contains("<pre><code"), "Streaming math fence remains code.");
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

Console.WriteLine($"Transcript Markdown smoke: {checks} checks passed.");
