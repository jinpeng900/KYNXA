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
var sizedDelimiters = new[] {
    @"S(0)+S(0)=S\big(S(0)+0\big)",
    @"S\big(S(0)+0\big)=S\big(S(0)\big)",
    @"\big(x\big)", @"\Big(x\Big)", @"\bigg(x\bigg)", @"\Bigg(x\Bigg)",
    @"\bigl(x\bigr)", @"\Bigl(x\Bigr)", @"\biggl(x\biggr)", @"\Biggl(x\Biggr)",
    @"\big\{x\big\}", @"\big|x\big|", @"\Bigl|x\Bigr|",
    @"\bigl[0,1\bigr)", @"\bigl. x\bigr|", @"\bigm|",
    @"\bigl(\frac{a}{b}\bigr)", @"\bigl(\left[x\right]\bigr)",
    @"\bigl(\Bigl[x\Bigr]\bigr)", @"{\bigl(x} + {y\bigr)}",
    @"\big(x", @"x\big)", @"\bigl\langle x\bigr\rangle", @"\Biggl\{x\Bigm|x>0\Biggr\}"
};
foreach (var latex in sizedDelimiters)
    foreach (bool block in new[] { false, true })
    {
        var rendered = MathFormulaRenderer.Render(latex, block);
        Check(rendered is not null, $"sized delimiter ({block}): {latex}");
        using var bitmap = SKBitmap.Decode(rendered!.Png);
        Check(bitmap.Pixels.Any(pixel => pixel.Alpha > 0), "visible sized delimiter glyphs");
    }
var sizedFraction = MathFormulaRenderer.Render(@"\bigl(\frac{a}{b}\bigr)", true)!;
var automaticFraction = MathFormulaRenderer.Render(@"\left(\frac{a}{b}\right)", true)!;
Check(sizedFraction.Png.SequenceEqual(automaticFraction.Png), "sized delimiters scale with fraction");
var separateGroups = MathFormulaRenderer.Render(@"{\bigl(x} + {y\bigr)}", false)!;
var unscaledGroups = MathFormulaRenderer.Render(@"{(x} + {y)}", false)!;
Check(separateGroups.Png.SequenceEqual(unscaledGroups.Png), "sizing does not pair across groups");
Check(MathFormulaRenderer.Render(@"\bigl(x\unknowncommand{y}\bigr)", false) is null, "unknown command stays unsupported after adaptation");
Check(MathFormulaRenderer.Render(@"\bigger(x)", false) is null, "unknown command sharing big prefix");
Check(MathFormulaRenderer.Render(@"\big x", false) is null, "sizing requires delimiter");
var aliases = new (string Source, string Native)[] {
    (@"\dfrac{a}{b}+\frac{c}{d}", @"{\displaystyle\frac{a}{b}}+\frac{c}{d}"),
    (@"\tfrac{a}{b}+\frac{c}{d}", @"{\textstyle\frac{a}{b}}+\frac{c}{d}"),
    (@"\dfrac{1+\tfrac{x}{y}}{z}", @"{\displaystyle\frac{1+{\textstyle\frac{x}{y}}}{z}}"),
    (@"\dfrac 12", @"{\displaystyle\frac 12}"),
    (@"\tfrac\alpha\beta", @"{\textstyle\frac\alpha\beta}"),
    (@"\dfrac{\{x\}}{y}", @"{\displaystyle\frac{\{x\}}{y}}"),
    (@"\begin{align}a&=b\\c&=d\end{align}", @"\begin{aligned}a&=b\\c&=d\end{aligned}"),
    (@"\begin{align*}a&=b\\c&=d\end{align*}", @"\begin{aligned}a&=b\\c&=d\end{aligned}")
};
foreach (var (sourceFormula, nativeFormula) in aliases)
    foreach (bool block in new[] { false, true })
    {
        var adapted = MathFormulaRenderer.Render(sourceFormula, block);
        var native = MathFormulaRenderer.Render(nativeFormula, block);
        Check(adapted is not null && native is not null, "compatible alias: " + sourceFormula);
        Check(adapted!.Png.SequenceEqual(native!.Png), "alias layout/style scope: " + sourceFormula);
    }
Check(MathFormulaRenderer.Render(@"\dfrac{x}", false) is null, "incomplete fraction stays source");
Check(MathFormulaRenderer.Render(@"\dfrac{\unknowncommand{x}}{y}", false) is null, "fraction does not hide unknown commands");
var modularFormulas = new (string Source, string Block, string Inline)[] {
    (@"1+1 \equiv 0 \pmod 2", @"1+1 \equiv 0 \quad(\mathrm{mod}\,\, 2)", @"1+1 \equiv 0 \;\,(\mathrm{mod}\,\, 2)"),
    (@"1+1 \equiv 2 \pmod 3", @"1+1 \equiv 2 \quad(\mathrm{mod}\,\, 3)", @"1+1 \equiv 2 \;\,(\mathrm{mod}\,\, 3)"),
    (@"1+1 \equiv 0 \pmod 1", @"1+1 \equiv 0 \quad(\mathrm{mod}\,\, 1)", @"1+1 \equiv 0 \;\,(\mathrm{mod}\,\, 1)"),
    (@"a\equiv b\pmod{n+1}", @"a\equiv b\quad(\mathrm{mod}\,\,{n+1})", @"a\equiv b\;\,(\mathrm{mod}\,\,{n+1})"),
    (@"a\pmod\alpha", @"a\quad(\mathrm{mod}\,\,\alpha)", @"a\;\,(\mathrm{mod}\,\,\alpha)"),
    (@"a\bmod b", @"a\:\mathrm{mod}\: b", @"a\:\mathrm{mod}\: b"),
    (@"a\bmod{n+1}", @"a\:\mathrm{mod}\:{n+1}", @"a\:\mathrm{mod}\:{n+1}"),
    (@"a\mod n", @"a\quad\mathrm{mod}\,\, n", @"a\:\:\:\mathrm{mod}\,\, n"),
    (@"a\mod{n+1}", @"a\quad\mathrm{mod}\,\,{n+1}", @"a\:\:\:\mathrm{mod}\,\,{n+1}"),
    (@"a\pod n", @"a\quad( n)", @"a\;\,( n)"),
    (@"a\pod{n+1}", @"a\quad({n+1})", @"a\;\,({n+1})"),
    (@"a\pmod{\dfrac{n}{2}}", @"a\quad(\mathrm{mod}\,\,{{\displaystyle\frac{n}{2}}})", @"a\;\,(\mathrm{mod}\,\,{{\displaystyle\frac{n}{2}}})"),
    (@"\dfrac{a\pmod 2}{b}", @"{\displaystyle\frac{a\quad(\mathrm{mod}\,\, 2)}{b}}", @"{\displaystyle\frac{a\;\,(\mathrm{mod}\,\, 2)}{b}}"),
    (@"\bigl(a\bmod{n}\bigr)", @"\left(a\:\mathrm{mod}\:{n}\right)", @"\left(a\:\mathrm{mod}\:{n}\right)"),
    (@"a\pmod{\{n\}}", @"a\quad(\mathrm{mod}\,\,{\{n\}})", @"a\;\,(\mathrm{mod}\,\,{\{n\}})"),
    (@"a\pmod{\pod{n}}", @"a\quad(\mathrm{mod}\,\,{\quad({n})})", @"a\;\,(\mathrm{mod}\,\,{\;\,({n})})")
};
foreach (var (sourceFormula, blockFormula, inlineFormula) in modularFormulas)
    foreach (bool block in new[] { false, true })
    {
        var adapted = MathFormulaRenderer.Render(sourceFormula, block);
        var native = MathFormulaRenderer.Render(block ? blockFormula : inlineFormula, block);
        Check(adapted is not null && native is not null, "modular formula: " + sourceFormula);
        Check(adapted!.Png.SequenceEqual(native!.Png), "modular text/parentheses/spacing: " + sourceFormula);
    }
foreach (string invalid in new[] { @"a\pmod", @"a\pod", @"a\mod", @"a\pmod{n", @"a\pmod}",
    @"a\pmodel{n}", @"a\bmodulus n", @"a\modulo n", @"a\podcast n", @"a\pmod{\unknowncommand{n}}" })
    Check(MathFormulaRenderer.Render(invalid, false) is null, "invalid modular command stays source: " + invalid);
foreach (string escaped in new[] { @"\\pmod 2", @"\\bmod n", @"\\mod{n}", @"\\pod{n}" })
{
    var painter = new CSharpMath.SkiaSharp.MathPainter { LaTeX = escaped, FontSize = 28, LineStyle = CSharpMath.Atom.LineStyle.Display };
    var measurement = painter.Measure();
    var rendered = MathFormulaRenderer.Render(escaped, true);
    Check(rendered is not null && rendered.Width == (Math.Ceiling(measurement.Width) + 8) / 2,
        "escaped modular command stays literal: " + escaped);
}
var standardFormulas = new[] {
    @"\displaystyle\frac{a}{b}", @"\text{hello world}",
    @"\begin{aligned}a&=b\\c&=d\end{aligned}",
    @"\begin{cases}x&x>0\\-x&x\leq0\end{cases}",
    @"\begin{matrix}a&b\\c&d\end{matrix}", @"\begin{bmatrix}a&b\\c&d\end{bmatrix}",
    @"\begin{Bmatrix}a&b\\c&d\end{Bmatrix}", @"\begin{vmatrix}a&b\\c&d\end{vmatrix}",
    @"\begin{Vmatrix}a&b\\c&d\end{Vmatrix}", @"\operatorname{rank}(A)", @"\mathbb{R}",
    @"\\big(x\\big)"
};
foreach (var latex in standardFormulas)
{
    var rendered = MathFormulaRenderer.Render(latex, true);
    Check(rendered is not null, "standard formula remains supported: " + latex);
    using var bitmap = SKBitmap.Decode(rendered!.Png);
    Check(bitmap.Pixels.Any(pixel => pixel.Alpha > 0), "visible standard formula glyphs");
}
var escapedPainter = new CSharpMath.SkiaSharp.MathPainter { LaTeX = @"\\big(x\\big)", FontSize = 28, LineStyle = CSharpMath.Atom.LineStyle.Display };
var escapedMeasurement = escapedPainter.Measure();
var escapedRendered = MathFormulaRenderer.Render(@"\\big(x\\big)", true)!;
Check(escapedRendered.Width == (Math.Ceiling(escapedMeasurement.Width) + 8) / 2,
    "escaped backslash does not turn literal big into a command");
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
const string delimiterCode = "`\\big(x\\big)`\n```tex\n\\[\\bigl(x\\bigr)\\]\n```";
Check(MathMarkdown.Normalize(delimiterCode) == delimiterCode, "sized delimiter commands inside code unchanged");
const string modularCode = "`\\pmod 2`\n```tex\n\\[1+1 \\equiv 0 \\pmod 2\\]\n```";
Check(MathMarkdown.Normalize(modularCode) == modularCode, "modular commands inside code unchanged");
foreach (string lineBreak in new[] { "\n", "\r\n" })
{
    Check(MathMarkdown.Normalize("\\(a +" + lineBreak + "  b = c\\)") == "$a + b = c$",
        "inline formula wraps within a paragraph");
    foreach (string boundary in new[] { lineBreak + lineBreak, lineBreak + " \t" + lineBreak,
        lineBreak + "```python" + lineBreak, lineBreak + "~~~python" + lineBreak, " `" })
    {
        string incomplete = "说明 \\( 未闭合" + boundary + "print(r\"\\)\")";
        Check(MathMarkdown.Normalize(incomplete) == incomplete, "inline formula cannot consume paragraph/code boundary");
    }
}
const string incompleteBeforeCode = "说明 \\( 未闭合\n\n```python\nprint(r\"\\)\")\n```";
Check(MathMarkdown.Normalize(incompleteBeforeCode) == incompleteBeforeCode, "unclosed formula preserves subsequent fenced code");
Check(MathMarkdown.Normalize(incompleteBeforeCode + "\n\n公式 \\(x=1\\)") == incompleteBeforeCode + "\n\n公式 $x=1$",
    "unclosed formula does not prevent later valid formulas");
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
// Persistence now belongs to the gateway; HTTP snapshot/order checks live in conversation-store-smoke.
// 正式持久化由网关负责；HTTP 快照与排序验证位于 conversation-store-smoke。
Console.WriteLine($"PASS: 8 formula layouts, {sizedDelimiters.Length} sized delimiters, {aliases.Length} aliases and {modularFormulas.Length} modular formulas in inline/block layouts, {standardFormulas.Length} standard/escaped formulas, scalable delimiter images, scope/unknown-command guards, source fallback, delimiters/code protection and recent/pinned/manual ordering.");
