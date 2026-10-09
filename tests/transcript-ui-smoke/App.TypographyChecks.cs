using System.Text.Json;
using KYNXA_Desktop.Services;

namespace TranscriptUiSmoke;

public partial class App
{
    // Inspect real glyph faces as well as CSS: a fallback font name alone does not prove it was used.
    // 同时检查真实字形字体和 CSS；仅看到回退字体名称不能证明实际使用了该字体。
    private async Task CheckTypographyAsync()
    {
        string originalLanguage = UiText.Language;
        var originalSize = _window!.AppWindow.Size;
        try
        {
            UiText.Initialize("zh-CN");
            _transcript.ClearSelection();
            var chat = Guid.NewGuid();
            var user = Message(chat, "user", "用户消息 User message，发送时间 Sent time");
            var answer = Message(chat, "assistant", "# 字体检查 Typography\n\n中文界面 English interface 0123456789，标点清楚。\n\n"
                + "| 中文名称 | English value |\n| --- | --- |\n| 字体 | Segoe UI Variable Text |\n\n"
                + "```cs\nvar count = 123; // 中文注释\n```\n\n公式 $x^2+\\alpha=1$，中文公式 $\\text{速度}=v$。\n\n"
                + MermaidFence("flowchart LR\n A[开始 Start] --> B[完成 Done]"));
            var sequence = Message(chat, "assistant", MermaidFence("sequenceDiagram\n participant UI as 桌面 User Interface\n participant API as 网关 Gateway\n UI->>API: 发送 Send message\n Note over UI,API: 备注 Note text"));
            string original = JsonSerializer.Serialize(new[] { user.Message, answer.Message, sequence.Message });
            _transcript.ShowConversation(chat, [user, answer, sequence]);
            await WaitAsync(DiagramBlockExpression(answer) + "?.dataset.diagramState === 'ready' && " + DiagramBlockExpression(sequence)
                + "?.dataset.diagramState === 'ready' && document.querySelectorAll('.katex').length === 2 && document.fonts.status === 'loaded'",
                "mixed Chinese/English, code, formulas and diagram finish rendering before font inspection");
            await EvalAsync<bool>("(() => { scrollTo(0, 0); return true; })()");
            var typography = await EvalAsync<JsonElement>("""
                (() => {
                  const selectors = ['.user .message-body', '.assistant .message-body > p', '.message-body h1',
                    '.message-body th', '.message-time', '.copy-message', '.mermaid-label', '.mermaid-svg text'];
                  return selectors.map(selector => {
                    const element = document.querySelector(selector), style = element && getComputedStyle(element);
                    return { selector, font: style?.fontFamily, size: style?.fontSize, weight: style?.fontWeight };
                  });
                })()
                """);
            _metrics["typography"] = typography;
            foreach (var item in typography.EnumerateArray())
            {
                string font = item.GetProperty("font").GetString() ?? "";
                Check(font.IndexOf("Segoe UI Variable Text", StringComparison.Ordinal) == 0 || font.StartsWith("\"Segoe UI Variable Text\"", StringComparison.Ordinal),
                    item.GetProperty("selector").GetString() + " uses the shared Latin-first UI stack");
                Check(font.Contains("Microsoft YaHei UI", StringComparison.Ordinal), "the same UI stack retains the Chinese font fallback");
            }

            await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("DOM.enable", "{}");
            await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("CSS.enable", "{}");
            var paragraphFonts = await PlatformFontsAsync(".assistant .message-body > p");
            _metrics["actualParagraphFonts"] = paragraphFonts;
            Check(HasPlatformFont(paragraphFonts, "Segoe UI Variable") && paragraphFonts.EnumerateArray().Any(font =>
                font.GetProperty("postScriptName").GetString() == "Segoe-UI-Variable-Text"),
                "English paragraph glyphs actually use installed Segoe UI Variable Text");
            Check(HasPlatformFont(paragraphFonts, "Microsoft YaHei UI"), "Chinese paragraph glyphs actually use installed Microsoft YaHei UI");
            var codeFonts = await PlatformFontsAsync(".message-body pre code");
            _metrics["actualCodeFonts"] = codeFonts;
            Check(HasPlatformFont(codeFonts, "Cascadia Mono") || HasPlatformFont(codeFonts, "Consolas"), "code glyphs retain their monospaced face");
            Check(await EvalAsync<bool>("getComputedStyle(document.querySelector('.katex .mord.mathnormal')).fontFamily.includes('KaTeX')"),
                "mathematical glyphs keep their dedicated KaTeX face");
            var sequenceFonts = await EvalAsync<JsonElement>($$"""
                [...{{DiagramBlockExpression(sequence)}}.querySelectorAll('.mermaid-viewport svg text')]
                  .filter(text => text.textContent.trim())
                  .map(text => ({ text: text.textContent, font: getComputedStyle(text).fontFamily }))
                """);
            _metrics["sequenceTypography"] = sequenceFonts;
            Check(sequenceFonts.GetArrayLength() >= 4 && sequenceFonts.EnumerateArray().All(text =>
                (text.GetProperty("font").GetString() ?? "").Contains("Segoe UI Variable Text", StringComparison.Ordinal)),
                "sequence actor, message and note labels use the shared font instead of their independent defaults");

            var sizes = new List<JsonElement>();
            foreach (int width in new[] { 980, 480 })
            {
                _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(width, 850));
                await Task.Delay(220);
                await EvalAsync<bool>("(() => { scrollTo(0, 0); return true; })()");
                var geometry = await EvalAsync<JsonElement>("({ viewport: innerWidth, page: document.documentElement.scrollWidth, heading: getComputedStyle(document.querySelector('h1')).fontSize, body: getComputedStyle(document.querySelector('.message-body > p')).fontSize })");
                sizes.Add(geometry);
                Check(geometry.GetProperty("page").GetDouble() <= geometry.GetProperty("viewport").GetDouble() + 1,
                    "unified fonts do not widen the transcript at native width " + width);
                Check(geometry.GetProperty("heading").GetString() != geometry.GetProperty("body").GetString(), "font unification preserves heading/body size hierarchy");
                await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), $"kynxa-transcript-typography-{width}.png"));
            }
            _metrics["typographyGeometry"] = sizes;
            await EvalAsync<bool>("(() => { document.querySelector('.mermaid-block [data-diagram-action=fullscreen]').click(); return true; })()");
            await WaitAsync("document.querySelector('dialog.mermaid-viewer')?.open === true", "diagram viewer opens for typography inspection");
            Check(await EvalAsync<bool>("getComputedStyle(document.querySelector('.mermaid-viewer-viewport svg text')).fontFamily.includes('Segoe UI Variable Text')"),
                "fullscreen diagram labels keep the shared font stack");
            Check(JsonSerializer.Serialize(new[] { user.Message, answer.Message, sequence.Message }) == original,
                "typography checks leave original message content and formal fields unchanged");
        }
        finally
        {
            await EvalAsync<bool>("(() => { window.kynxaDiagrams?.closeViewer(); return true; })()");
            UiText.Initialize(originalLanguage);
            _window.AppWindow.Resize(originalSize);
        }
    }

    private async Task<JsonElement> PlatformFontsAsync(string selector)
    {
        using var document = JsonDocument.Parse(await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("DOM.getDocument", "{}"));
        int root = document.RootElement.GetProperty("root").GetProperty("nodeId").GetInt32();
        using var node = JsonDocument.Parse(await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("DOM.querySelector", JsonSerializer.Serialize(new { nodeId = root, selector })));
        int id = node.RootElement.GetProperty("nodeId").GetInt32();
        Check(id > 0, "real font inspection finds " + selector);
        using var fonts = JsonDocument.Parse(await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("CSS.getPlatformFontsForNode", JsonSerializer.Serialize(new { nodeId = id })));
        return fonts.RootElement.GetProperty("fonts").Clone();
    }

    private static bool HasPlatformFont(JsonElement fonts, string family) => fonts.EnumerateArray().Any(font =>
        (font.GetProperty("familyName").GetString() ?? "").Contains(family, StringComparison.OrdinalIgnoreCase) &&
        font.GetProperty("glyphCount").GetInt32() > 0);
}
