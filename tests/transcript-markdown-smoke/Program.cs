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
Check(TranscriptMarkdown.Render(fence, true).Contains("<pre data-code-complete=\"true\"><code"), "Streaming math fence remains code.");
Check(TranscriptMarkdown.Render(fence).Contains("class=\"math\""), "Settled math fence becomes display math.");
Check(!TranscriptMarkdown.Render("```tex\n$\\theta$\n```").Contains("class=\"math\""), "Ordinary TeX code is not interpreted as math.");

string source = "const text = \"<script>alert(1)</script>\";\nconst answer = 42;";
string code = TranscriptMarkdown.Render("```js\n" + source + "\n```");
Check(code.Contains("<span style=\"color:#"), "Supported code receives token colors.");
Check(Text(code).TrimEnd('\r', '\n') == source, "Highlighting preserves the entire copied source.");
Check(!code.Contains("<script>"), "Code is escaped.");
Check(code.Contains("data-code-complete=\"true\""), "A closed ordinary fence is complete.");
Check(TranscriptMarkdown.Render("```text\nready\n```\n\nStill streaming", true).Contains("data-code-complete=\"true\""),
    "An earlier closed block remains complete while later prose streams.");
Check(TranscriptMarkdown.Render("```unknown\n  partial\t中文", true).Contains("data-code-complete=\"false\""),
    "An unfinished block offers its current visible content.");
Check(TranscriptMarkdown.Render("```text\npartial").Contains("data-code-complete=\"false\""),
    "Stopping an unclosed fence does not mark its content complete.");
Check(TranscriptMarkdown.Render("    value", true).Contains("data-code-complete=\"false\""),
    "Indented code waits until streaming finishes before being marked complete.");
Check(TranscriptMarkdown.Render("    value").Contains("data-code-complete=\"true\""),
    "Settled indented code is complete.");

// Diagram classification depends on the actual Markdown closer, not the reply's streaming state.
// 图表分类依据 Markdown 的真实闭合围栏，不能将流式结束或取消误当作源码已经完整。
string mermaidSource = "flowchart TD\n  A[\"中文 English <script>alert(1)</script> & \\\"quoted\\\"\"] --> B[\"下一步<br/>继续\"]\n\n  B --> C[\"完成\"]";
string mermaidFence = "```mermaid\n" + mermaidSource + "\n```";
string mermaid = TranscriptMarkdown.Render(mermaidFence);
Check(mermaid.StartsWith("<div class=\"mermaid-block\" data-mermaid-ready=\"true\"><pre class=\"mermaid-source\"><code class=\"language-mermaid\" data-language=\"mermaid\">")
    && mermaid.TrimEnd().EndsWith("</code></pre></div>"), "Complete Mermaid is emitted as a standalone source-preserving diagram block.");
Check(Text(mermaid).TrimEnd('\r', '\n') == mermaidSource, "Mermaid keeps Chinese, English, indentation, empty lines and literal label markup.");
Check(!mermaid.Contains("<script>") && !mermaid.Contains("<br/>") && !mermaid.Contains("<span")
    && mermaid.Contains("&lt;script&gt;") && mermaid.Contains("&amp;"), "Mermaid source is escaped text without highlighting or executable label markup.");
Check(Regex.Matches(mermaid, "data-[a-z-]+=\"[^\"]*\"").Count == 2,
    "Mermaid attributes contain readiness and a fixed language, never user source.");
Check(TranscriptMarkdown.Render(mermaidFence, true) == mermaid, "A closed Mermaid fence is ready even while the remaining reply is streaming.");
string incompleteMermaid = "```mermaid\nflowchart TD\n  A[\"未完成";
Check(TranscriptMarkdown.Render(incompleteMermaid, true).Contains("data-mermaid-ready=\"false\""),
    "An incomplete streaming Mermaid fence waits as source.");
Check(TranscriptMarkdown.Render(incompleteMermaid).Contains("data-mermaid-ready=\"false\""),
    "An incomplete final or cancelled Mermaid fence remains source.");
Check(Text(TranscriptMarkdown.Render(incompleteMermaid)).TrimEnd('\r', '\n') == "flowchart TD\n  A[\"未完成",
    "Incomplete Mermaid source is retained without a guessed closing delimiter.");
Check(TranscriptMarkdown.Render("````mermaid\nflowchart TD\n  A --> B\n```").Contains("data-mermaid-ready=\"false\""),
    "A shorter closing fence does not mark a Mermaid block complete.");
Check(TranscriptMarkdown.Render("```mermaid\nflowchart TD\n  A --> B\n````").Contains("data-mermaid-ready=\"true\""),
    "A longer matching closing fence is accepted.");
Check(TranscriptMarkdown.Render("~~~mermaid\nsequenceDiagram\n  Alice->>Bob: 你好\n~~~").Contains("data-mermaid-ready=\"true\""),
    "Tilde Mermaid fences are supported.");
Check(TranscriptMarkdown.Render("~~~mermaid\nflowchart TD\n  A --> B\n```").Contains("data-mermaid-ready=\"false\""),
    "A different fence character cannot close Mermaid source.");
Check(TranscriptMarkdown.Render("```MeRmAiD title\nflowchart TD\n  A --> B\n```").Contains("data-mermaid-ready=\"true\""),
    "The Mermaid language token is case insensitive and accepts following info text.");
Check(!TranscriptMarkdown.Render("```mermaidish\nflowchart TD\n  A --> B\n```").Contains("mermaid-block"),
    "Similar code language names are not classified as Mermaid.");
Check(!TranscriptMarkdown.Render("    mermaid\n    flowchart TD\n      A --> B").Contains("mermaid-block"),
    "Indented ordinary code is not classified as a Mermaid fence.");
Check(Text(TranscriptMarkdown.Render(mermaidFence.Replace("\n", "\r\n"))).TrimEnd('\r', '\n') == mermaidSource.Replace("\n", "\r\n"),
    "Mermaid source keeps the original CRLF line endings and indentation.");
string multipleDiagrams = TranscriptMarkdown.Render(mermaidFence + "\n\n```js\nconst answer = 42;\n```\n\n~~~mermaid\nclassDiagram\n  Animal <|-- Cat\n~~~");
Check(Regex.Matches(multipleDiagrams, "class=\"mermaid-block\"").Count == 2
    && Regex.Matches(multipleDiagrams, "data-mermaid-ready=\"true\"").Count == 2,
    "Multiple diagrams are independent blocks within one reply.");
Check(multipleDiagrams.Contains("data-language=\"js\"") && multipleDiagrams.Contains("<span style=\"color:#"),
    "Ordinary highlighted code remains available beside Mermaid diagrams.");
foreach (string diagramType in new[] { "flowchart TD", "graph LR", "sequenceDiagram", "stateDiagram-v2", "classDiagram", "erDiagram", "gantt", "mindmap" })
{
    string classifiedDiagram = TranscriptMarkdown.Render("```mermaid\n" + diagramType + "\n```");
    Check(classifiedDiagram.Contains("data-mermaid-ready=\"true\"") && Text(classifiedDiagram).TrimEnd('\r', '\n') == diagramType,
        $"Mermaid diagram type {diagramType} passes unchanged to the browser renderer.");
}

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
