using KYNXA_Desktop.Services;
using KYNXA_Desktop.Models.UI;
using Markdig;
using Markdig.Extensions.Mathematics;
using SkiaSharp;

static void Check(bool value, string name) { if (!value) throw new Exception(name); }
var formulas = new[] { "1+1=S(1)=2", @"0=\varnothing", @"1+1\cong 2",
    @"\frac{a+b}{c}", @"\sqrt{x^2+y^2}", @"\sum_{i=1}^{n}i", @"\int_0^1 x^2\,dx",
    @"\begin{pmatrix}a&b\\c&d\end{pmatrix}" };
foreach (var latex in formulas)
{
    var rendered = MathFormulaRenderer.Render(latex, true);
    Check(rendered is not null, "formula: " + latex);
    using var bitmap = SKBitmap.Decode(rendered!.Png);
    Check(bitmap.Width > 0 && bitmap.Pixels.Any(pixel => pixel.Alpha > 0), "visible glyphs");
}
Check(MathFormulaRenderer.Render(@"\unknowncommand{x}", false) is null, "unsupported command fallback");
Check(MathFormulaRenderer.Render(new string('{', 40) + "x", false) is null, "depth limit");
Check(MathFormulaRenderer.Render(new string('x', 2049), false) is null, "length limit");
string source = """
    Inline \(1+1=2\), and $x^2$.
    \[
    \frac{1}{2}
    \]
    [
    1+1=0
    ]
    `\(not math\)`
    ```python
    print(r"\[not math\]")
    ```
    """;
var normalized = MathMarkdown.Normalize(source);
Check(normalized.Contains("$1+1=2$"), "inline delimiter");
Check(normalized.Contains("`\\(not math\\)`"), "inline code unchanged");
Check(normalized.Contains("print(r\"\\[not math\\]\")"), "fenced code unchanged");
Check(Markdown.Parse(normalized, new MarkdownPipelineBuilder().UseMathematics().Build())
    .OfType<MathBlock>().Count() == 2, "display blocks");
Check(MathMarkdown.Normalize("[\nordinary paragraph\n]") == "[\nordinary paragraph\n]", "ordinary brackets");
var pinned = new ProjectState { Name = "置顶", IsPinned = true };
var a = new ProjectState { Name = "A" };
var b = new ProjectState { Name = "B" };
var projects = new List<ProjectState> { pinned, a, b };
ProjectOrdering.Activate(projects, b);
Check(projects.OrderByDescending(p => p.IsPinned).SequenceEqual(new[] { pinned, b, a }), "recent below pinned");
Check(!ProjectOrdering.Activate(projects, pinned), "pinned order stable");
Check(ProjectOrdering.Move(projects, b, a, true), "manual reorder");
Check(projects.OrderByDescending(p => p.IsPinned).SequenceEqual(new[] { pinned, a, b }), "drop below target");
Check(!ProjectOrdering.Move(projects, a, pinned, false), "cannot cross pinned group");
var directory = Path.Combine(Path.GetTempPath(), "kynxa-order-test-" + Guid.NewGuid().ToString("N"));
try
{
    var store = new ProjectStore(directory);
    store.Save(projects);
    Check(store.Load().Select(p => p.Id).SequenceEqual(projects.Select(p => p.Id)), "persist manual order");
}
finally { if (File.Exists(Path.Combine(directory, "projects.json"))) File.Delete(Path.Combine(directory, "projects.json")); if (Directory.Exists(directory)) Directory.Delete(directory); }
Console.WriteLine("PASS: 8 formula layouts, source fallback, delimiters/code protection, recent/pinned/manual ordering and persistence.");
