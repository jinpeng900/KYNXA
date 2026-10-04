using CSharpMath.SkiaSharp;
using SkiaSharp;
using System.Text;
using System.Text.RegularExpressions;

namespace KYNXA_Desktop.Services;

public sealed record FormulaImage(byte[] Png, double Width, double Height, double Baseline);

public static class MathFormulaRenderer
{
    private static readonly Regex LatexTokens = new(@"\\[a-zA-Z]+|\\[^a-zA-Z]|[{}]", RegexOptions.Compiled);
    private static readonly Regex SizedCommand = new(@"^\\(?:big|Big|bigg|Bigg)[lrm]?$", RegexOptions.Compiled);
    private static readonly Regex Delimiter = new(@"\G\s*(?<value>\\(?:langle|rangle|lbrace|rbrace|lvert|rvert|lVert|rVert|lfloor|rfloor|lceil|rceil|vert|Vert|uparrow|downarrow|updownarrow|Uparrow|Downarrow|Updownarrow|backslash|[{}|])|[()[\]|/.<>])", RegexOptions.Compiled);
    private static readonly Regex AlignEnvironment = new(@"\G\s*\{align\*?\}", RegexOptions.Compiled);

    private static string NormalizeAliases(string latex, bool block)
    {
        var edits = new List<(int Start, int Length, string Value)>();
        foreach (Match token in LatexTokens.Matches(latex))
        {
            int end = token.Index + token.Length;
            if (token.Value is @"\dfrac" or @"\tfrac")
            {
                // Group the complete fraction so its style cannot leak into later terms.
                // 整体包裹分数，避免其样式泄漏到后续项。
                int numeratorEnd = ArgumentEnd(latex, end);
                int denominatorEnd = numeratorEnd < 0 ? -1 : ArgumentEnd(latex, numeratorEnd);
                if (denominatorEnd < 0) continue;
                string style = token.Value == @"\dfrac" ? @"\displaystyle" : @"\textstyle";
                edits.Add((token.Index, token.Length, "{" + style + @"\frac"));
                edits.Add((denominatorEnd, 0, "}"));
            }
            else if (token.Value is @"\begin" or @"\end")
            {
                var environment = AlignEnvironment.Match(latex, end);
                if (environment.Success)
                    edits.Add((end, environment.Length, "{aligned}"));
            }
            else if (token.Value == @"\bmod")
            {
                // Binary remainder operator: upright text with medium math spacing.
                // 二元余数运算符使用直立文本与中等数学间距。
                edits.Add((token.Index, token.Length, @"\:\mathrm{mod}\:"));
            }
            else if (token.Value is @"\pmod" or @"\pod" or @"\mod")
            {
                int argumentEnd = ArgumentEnd(latex, end);
                if (argumentEnd < 0) continue;
                // Congruence annotations consume one TeX argument. Keep it in math
                // typeface; only "mod" is upright. Display annotations get more space.
                // 同余标注消耗一个 TeX 参数并保持数学字体，只有 mod 直立；块级标注增加间距。
                bool parenthesized = token.Value != @"\mod";
                string spacing = block ? @"\quad" : parenthesized ? @"\;\," : @"\:\:\:";
                string label = token.Value == @"\pod" ? "" : @"\mathrm{mod}\,\,";
                edits.Add((token.Index, token.Length, spacing + (parenthesized ? "(" : "") + label));
                if (parenthesized) edits.Add((argumentEnd, 0, ")"));
            }
        }
        var result = new StringBuilder(latex);
        foreach (var edit in edits.OrderByDescending(edit => edit.Start))
            result.Remove(edit.Start, edit.Length).Insert(edit.Start, edit.Value);
        return result.ToString();
    }

    private static int ArgumentEnd(string latex, int start)
    {
        while (start < latex.Length && char.IsWhiteSpace(latex[start])) start++;
        if (start == latex.Length || latex[start] == '}') return -1;
        if (latex[start] != '{')
        {
            if (latex[start] != '\\') return start + 1;
            var command = LatexTokens.Match(latex, start);
            return command.Success && command.Index == start ? start + command.Length : -1;
        }
        int depth = 1;
        for (int i = start + 1; i < latex.Length; i++)
        {
            if (latex[i] == '\\')
            {
                var command = LatexTokens.Match(latex, i);
                if (command.Success && command.Index == i) i += command.Length - 1;
            }
            else if (latex[i] == '{') depth++;
            else if (latex[i] == '}' && --depth == 0) return i + 1;
        }
        return -1;
    }

    // CSharpMath supports automatic delimiters but not TeX's fixed-size big/Big family.
    // Adapt only the image source. Stored and selectable LaTeX remains unchanged.
    // CSharpMath 支持自动定界符，但不支持 TeX 固定尺寸的 big/Big 系列；只适配图片源文，保存与可选择的 LaTeX 不变。
    private static string NormalizeSizedDelimiters(string latex)
    {
        var edits = new List<(int Start, int Length, string Delimiter, char Role, int Scope)>();
        var scopes = new Stack<(char Kind, int Id)>();
        scopes.Push(('r', 0));
        int nextScope = 0, consumedUntil = 0;
        foreach (Match token in LatexTokens.Matches(latex))
        {
            if (token.Index < consumedUntil) continue;
            string command = token.Value;
            if (command is "{" or @"\left")
                scopes.Push((command == "{" ? 'g' : 'd', ++nextScope));
            else if (command is "}" or @"\right")
            {
                if (scopes.Count > 1 && scopes.Peek().Kind == (command == "}" ? 'g' : 'd')) scopes.Pop();
            }
            else if (SizedCommand.IsMatch(command))
            {
                var delimiterMatch = Delimiter.Match(latex, token.Index + token.Length);
                if (!delimiterMatch.Success) continue;
                string delimiter = delimiterMatch.Groups["value"].Value;
                char role = command[^1] is 'l' or 'r' or 'm' ? command[^1] : delimiter switch
                {
                    "(" or "[" or "<" or @"\{" or @"\lbrace" or @"\langle" or @"\lfloor" or @"\lceil" or @"\lvert" or @"\lVert" => 'l',
                    ")" or "]" or ">" or @"\}" or @"\rbrace" or @"\rangle" or @"\rfloor" or @"\rceil" or @"\rvert" or @"\rVert" => 'r',
                    "|" or @"\|" or @"\vert" or @"\Vert" => 'b',
                    _ => 'm'
                };
                consumedUntil = delimiterMatch.Index + delimiterMatch.Length;
                edits.Add((token.Index, consumedUntil - token.Index, delimiter, role, scopes.Peek().Id));
            }
        }
        if (edits.Count == 0) return latex;
        var replacements = new string[edits.Count];
        var pending = new Dictionary<int, Stack<int>>();
        for (int i = 0; i < edits.Count; i++)
        {
            var edit = edits[i];
            // An unmatched invisible delimiter is left to the original parser's fallback.
            // 无法配对的不可见定界符留给原解析器回退处理。
            replacements[i] = edit.Delimiter == "." ? latex.Substring(edit.Start, edit.Length) : edit.Delimiter;
            if (!pending.TryGetValue(edit.Scope, out var stack)) pending[edit.Scope] = stack = new();
            bool closes = edit.Role == 'r' || (edit.Role == 'b' && stack.Count > 0
                && edits[stack.Peek()].Role == 'b' && edits[stack.Peek()].Delimiter == edit.Delimiter);
            if (closes && stack.Count > 0)
            {
                int opening = stack.Pop();
                replacements[opening] = @"\left" + edits[opening].Delimiter;
                replacements[i] = @"\right" + edit.Delimiter;
            }
            else if (edit.Role is 'l' or 'b') stack.Push(i);
        }
        var result = new StringBuilder(latex.Length);
        int copiedUntil = 0;
        for (int i = 0; i < edits.Count; i++)
        {
            var edit = edits[i];
            result.Append(latex, copiedUntil, edit.Start - copiedUntil).Append(replacements[i]);
            copiedUntil = edit.Start + edit.Length;
        }
        return result.Append(latex, copiedUntil, latex.Length - copiedUntil).ToString();
    }

    public static FormulaImage? Render(string latex, bool block)
    {
        if (string.IsNullOrWhiteSpace(latex) || latex.Length > 2048) return null;
        int depth = 0;
        foreach (char c in latex)
        {
            if (c == '{' && ++depth > 32) return null;
            if (c == '}') depth--;
        }
        try
        {
            var painter = new MathPainter
            {
                LaTeX = NormalizeSizedDelimiters(NormalizeAliases(latex, block)), FontSize = block ? 28 : 24,
                LineStyle = block ? CSharpMath.Atom.LineStyle.Display : CSharpMath.Atom.LineStyle.Text,
                TextColor = new SKColor(35, 35, 35), DisplayErrorInline = false
            };
            if (painter.ErrorMessage is not null) return null;
            var size = painter.Measure();
            if (!float.IsFinite(size.Width) || !float.IsFinite(size.Height)
                || size.Width <= 0 || size.Height <= 0 || size.Width > 4096 || size.Height > 1024) return null;
            // Explicit padding avoids clipping ascenders/descenders at fractional pixel bounds.
            // 明确留出边距，避免小数像素边界裁切字符的上升部或下降部。
            int width = (int)Math.Ceiling(size.Width) + 8, height = (int)Math.Ceiling(size.Height) + 8;
            using var surface = SKSurface.Create(new SKImageInfo(width, height));
            surface.Canvas.Clear(SKColors.Transparent);
            painter.Draw(surface.Canvas, 4, 4 - size.Y);
            using var image = surface.Snapshot();
            using var data = image.Encode(SKEncodedImageFormat.Png, 100);
            return new FormulaImage(data.ToArray(), width / 2d, height / 2d, (4 - size.Y) / 2d);
        }
        catch (Exception error) when (error is ArgumentException or InvalidOperationException or NotSupportedException)
        {
            return null; // Unsupported/incomplete LaTeX remains readable source.
            // 不支持或未完成的 LaTeX 仍以可读源文展示。
        }
    }
}
