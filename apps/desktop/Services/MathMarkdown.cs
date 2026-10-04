using System.Text.RegularExpressions;

namespace KYNXA_Desktop.Services;

public static class MathMarkdown
{
    // Keep fenced and inline code intact before translating the alternate LaTeX delimiters.
    // Inline formulas can wrap within a paragraph, but cannot cross blank lines or code.
    // 转换替代 LaTeX 定界符前保留围栏与行内代码；行内公式可在段落内换行，但不能跨空行或代码。
    private static readonly Regex Delimiters = new(
        @"(?<code>(?<fence>`{3,}|~{3,})[^\n]*\n[\s\S]*?(?:\k<fence>|$)|(?<ticks>`+)[^\r\n]*?\k<ticks>)|\\\[(?<block>[\s\S]*?)\\\]|\\\((?<inline>(?:[^\r\n`]|\r?\n(?![ \t]*(?:\r?\n|~{3,})))*?)\\\)|(?m:^[ \t]*\[[ \t]*\r?\n(?<bare>[^\[\]]{1,2048})\r?\n[ \t]*\][ \t]*$)",
        RegexOptions.CultureInvariant, TimeSpan.FromMilliseconds(150));

    public static string Normalize(string text)
    {
        try
        {
            return Delimiters.Replace(text, match =>
            {
                if (match.Groups["code"].Success) return match.Value;
                if (match.Groups["inline"].Success) return "$" + Regex.Replace(match.Groups["inline"].Value, @"\r?\n[ \t]*", " ").Trim() + "$";
                var content = match.Groups["block"].Success ? match.Groups["block"].Value : match.Groups["bare"].Value;
                // A standalone bracketed paragraph is only recovered when it looks mathematical.
                // 独立方括号段落只有在内容符合数学特征时才恢复为公式。
                if (match.Groups["bare"].Success && !Regex.IsMatch(content, @"[=+^_\\∈≅]")) return match.Value;
                return "\n$$\n" + content.Trim() + "\n$$\n";
            });
        }
        catch (RegexMatchTimeoutException) { return text; }
    }
}
