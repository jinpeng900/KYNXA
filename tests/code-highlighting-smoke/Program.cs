using KYNXA_Desktop.Services;

var cases = new (string Language, string Source)[]
{
    ("rust", "// 中文注释\nuse std::fmt;\npub fn greet(name: &str) -> String { format!(\"你好 {name}\") }"),
    ("diff", "diff --git a/src.rs b/src.rs\n--- a/src.rs\n+++ b/src.rs\n@@ -1 +1 @@\n-old\n+new\n"),
    ("tsx", "interface Item { title: string; }\nconst card = <div>Hello</div>;"),
    ("python", "# 中文注释 😊\r\ndef greet(name):\r\n\treturn '你好'\r\n"),
    ("js", "// note\nconst message = \"hello\";\nif (true) console.log(message);"),
    ("ts", "interface User { name: string; }\nconst user: User = { name: '用户' };"),
    ("c#", "// note\r\npublic class Demo { string value = \"hello\"; }\r\n"),
    ("cpp", "// note\nint main() { return 0; }"),
    ("java", "public class Demo { String name = \"hello\"; }"),
    ("json", "{\"name\": \"你好\", \"count\": 12, \"ok\": true}"),
    ("sql", "SELECT name FROM users WHERE id = 12; -- note"),
    ("html", "<!-- note --><div class=\"card\">你好 &amp; 世界</div>"),
    ("css", "/* note */ .card { color: red; margin: 12px; }"),
    ("xaml", "<Grid Background=\"White\"><TextBlock Text=\"你好\" /></Grid>"),
    ("ps1", "# note\n$greeting = 'hello'\nWrite-Output $greeting"),
    ("PYTHON3", "def greet():\n    return 'hello'"),
    ("language-python", "# note\ndef f(): return True"),
};
foreach (var (language, source) in cases)
{
    var tokens = CodeSyntaxHighlighter.Highlight(source, language);
    Check(string.Concat(tokens.Select(t => t.Text)) == source, language + " exact text preservation");
    Check(tokens.Any(t => t.Color.HasValue), language + " highlighting");
    Console.WriteLine("PASS " + language);
}

const string python = "# return '注释内的字符串'\nvalue = 'return # 字符串内的注释'\nreturn value";
var pythonTokens = CodeSyntaxHighlighter.Highlight(python, "py");
uint? ColorAt(int index)
{
    foreach (var token in pythonTokens)
    {
        if (index < token.Text.Length) return token.Color;
        index -= token.Text.Length;
    }
    return null;
}
Check(ColorAt(python.IndexOf("return", StringComparison.Ordinal)) == 0xFF576F43, "keyword inside comment stays comment color");
Check(ColorAt(python.IndexOf("return #", StringComparison.Ordinal)) == 0xFF0A3069, "keyword/comment marker inside string stays string color");
Check(ColorAt(python.LastIndexOf("return", StringComparison.Ordinal)) == 0xFF8250DF, "actual keyword gets keyword color");
foreach (string? language in new string?[] { null, "", "text", "unknown-language" })
{
    var tokens = CodeSyntaxHighlighter.Highlight(python, language);
    Check(tokens.Count == 1 && tokens[0].Text == python && tokens[0].Color is null, "plain text fallback");
}
foreach (string source in new[] { "", "\t\r\n", "def f():\n  return 'unfinished", new string('x', 40_001) })
    Check(string.Concat(CodeSyntaxHighlighter.Highlight(source, "python").Select(t => t.Text)) == source, "empty/incomplete/large source preservation");
string loadedPages = string.Concat(Enumerable.Repeat("pub fn greet() -> &str { \"你好\" }\n", 4500));
var loadedTokens = CodeSyntaxHighlighter.Highlight(loadedPages, "rust");
Check(loadedPages.Length > 128_000 && loadedTokens.Count > 16384
    && loadedTokens.LastOrDefault(token => token.Text.Contains("pub", StringComparison.Ordinal))?.Color is not null
    && string.Concat(loadedTokens.Select(token => token.Text)) == loadedPages,
    "loaded pages retain colors past the former character and token-run thresholds");
Console.WriteLine("PASS aliases, lexical contexts, CRLF/tabs/Unicode, plain fallback, incomplete/large input");

static void Check(bool condition, string message)
{
    if (!condition) throw new InvalidOperationException(message);
}
