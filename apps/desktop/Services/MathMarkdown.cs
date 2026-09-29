using System.Text.RegularExpressions;

namespace KYNXA_Desktop.Services;

public static class MathMarkdown
{
    // Keep fenced and inline code intact before translating the alternate LaTeX delimiters.
    private static readonly Regex Delimiters = new(
        @"(?<code>(?<fence>`{3,}|~{3,})[^\n]*\n[\s\S]*?(?:\k<fence>|$)|(?<ticks>`+)[^\r\n]*?\k<ticks>)|\\\[(?<block>[\s\S]*?)\\\]|\\\((?<inline>[^\r\n]*?)\\\)|(?m:^[ \t]*\[[ \t]*\r?\n(?<bare>[^\[\]]{1,2048})\r?\n[ \t]*\][ \t]*$)",
        RegexOptions.CultureInvariant, TimeSpan.FromMilliseconds(150));

    public static string Normalize(string text)
    {
        try
        {
            return Delimiters.Replace(text, match =>
            {
                if (match.Groups["code"].Success) return match.Value;
                if (match.Groups["inline"].Success) return "$" + match.Groups["inline"].Value + "$";
                var content = match.Groups["block"].Success ? match.Groups["block"].Value : match.Groups["bare"].Value;
                // A standalone bracketed paragraph is only recovered when it looks mathematical.
                if (match.Groups["bare"].Success && !Regex.IsMatch(content, @"[=+^_\\∈≅]")) return match.Value;
                return "\n$$\n" + content.Trim() + "\n$$\n";
            });
        }
        catch (RegexMatchTimeoutException) { return text; }
    }
}
