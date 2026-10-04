using ColorCode;
using ColorCode.Parsing;
using System.Text.RegularExpressions;

namespace KYNXA_Desktop.Services;

internal sealed record CodeToken(string Text, uint? Color = null);

/// <summary>
/// Produces colored text without HTML, executable content, or changes to copied code.
/// 生成带颜色文本，不引入 HTML、可执行内容或复制代码的变化。
/// </summary>
internal sealed class CodeSyntaxHighlighter : CodeColorizerBase
{
    private readonly List<CodeToken> _tokens = [];
    private CodeSyntaxHighlighter() : base(null!, null!) { }

    public static IReadOnlyList<CodeToken> Highlight(string source, string? language)
    {
        // Large/generated blocks remain selectable without thousands of UI text runs.
        // 大型或生成的代码块仍可选择，不创建数千个 UI 文本片段。
        if (source.Length > 40_000 || ResolveLanguage(language) is not { } syntax)
            return [new(source)];
        try
        {
            var formatter = new CodeSyntaxHighlighter();
            formatter.languageParser.Parse(source, syntax, formatter.Write);
            return string.Concat(formatter._tokens.Select(token => token.Text)) == source
                ? formatter._tokens : [new(source)];
        }
        catch (Exception error) when (error is ArgumentException or InvalidOperationException or RegexMatchTimeoutException)
        {
            // An unsupported/incomplete grammar must never prevent displaying the response.
            // 语法不支持或尚未完整时，不能阻止回复展示。
            return [new(source)];
        }
    }

    private static ILanguage? ResolveLanguage(string? info)
    {
        string id = (info ?? string.Empty).Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries).FirstOrDefault()?.ToLowerInvariant() ?? "";
        if (id.StartsWith("language-", StringComparison.Ordinal)) id = id[9..];
        return id switch
        {
            "py" or "python" or "python3" => Languages.Python,
            "js" or "javascript" => Languages.JavaScript,
            "ts" or "typescript" => Languages.Typescript,
            "cs" or "c#" or "csharp" => Languages.CSharp,
            "c" or "cc" or "cpp" or "c++" or "h" => Languages.Cpp,
            "html" or "htm" => Languages.Html,
            "xml" or "xaml" or "svg" => Languages.Xml,
            "ps1" or "pwsh" or "powershell" => Languages.PowerShell,
            "fs" or "f#" or "fsharp" => Languages.FSharp,
            "" or "text" or "plain" or "plaintext" or "txt" => null,
            _ => Languages.FindById(id),
        };
    }

    protected override void Write(string source, IList<Scope> scopes)
    {
        var colors = new uint?[source.Length];
        foreach (var scope in scopes) Paint(scope, colors);
        for (int start = 0; start < source.Length;)
        {
            int end = start + 1;
            while (end < source.Length && colors[end] == colors[start]) end++;
            _tokens.Add(new CodeToken(source[start..end], colors[start]));
            if (_tokens.Count > 4096) throw new InvalidOperationException("Code block exceeds the text-run budget.");
            start = end;
        }
    }

    private static void Paint(Scope scope, uint?[] colors)
    {
        if (GetColor(scope.Name) is { } color)
        {
            int start = Math.Clamp(scope.Index, 0, colors.Length);
            int end = (int)Math.Clamp((long)scope.Index + scope.Length, start, colors.Length);
            Array.Fill(colors, (uint?)color, start, end - start);
        }
        foreach (var child in scope.Children) Paint(child, colors);
    }

    // A restrained light palette with sufficient contrast on the reply's #F7F7F7 surface.
    // 浅色调色板在回复的 #F7F7F7 背景上保持足够对比度。
    private static uint? GetColor(string scope)
    {
        string name = scope.ToLowerInvariant();
        if (name.Contains("comment")) return 0xFF576F43;
        if (name.Contains("keyword") || name.Contains("const") || name.Contains("builtinvalue")) return 0xFF8250DF;
        if (name.Contains("string") || name.Contains("attributevalue")) return 0xFF0A3069;
        if (name.Contains("number")) return 0xFF0550AE;
        if (name.Contains("function") || name.Contains("command") || name.Contains("attribute") || name.Contains("jsonkey")) return 0xFF953800;
        if (name.Contains("type") || name.Contains("element") || name.Contains("tagname")) return 0xFF116329;
        if (name.Contains("operator") || name.Contains("delimiter")) return 0xFF57606A;
        return null;
    }
}
