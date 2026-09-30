using System.Text.Json;
using Microsoft.Web.WebView2.Core;
using Windows.Storage.Streams;

namespace TranscriptUiSmoke;

public partial class App
{
    private async Task CheckWrappingAndResizeAsync()
    {
        string token = string.Concat(Enumerable.Repeat("LONG_TOKEN_0123456789_", 8));
        string sourceCode = "# 自动折行不改变实际代码或复制结果\n"
            + "def configure_endpoint():\n"
            + "    endpoint = \"https://example.invalid/" + token + "\"\n"
            + "    payload = {\"label\": \"中文内容\", \"enabled\": True, \"retries\": 3}\n"
            + "    return endpoint, payload";
        string markdown = "## 自动换行与回复宽度\n\n```python\n" + sourceCode + "\n```\n\n"
            + "普通 Markdown 长链接：https://example.invalid/" + token + "\n\n"
            + "连续长单词：" + token + "\n\n"
            + "| 类型 | 表达式 | 描述 |\n| --- | --- | --- |\n"
            + "| 速度 | $v=s/t$ | " + token + " |\n"
            + "| 平方 | $x^2$ | 宽度跟随聊天区，代码复制保留原文 |";
        var chat = Guid.NewGuid();
        _transcript.ShowConversation(chat, new[]
        {
            Message(chat, "user", "请让长代码、长链接和表格在聊天区域内自动换行。"),
            Message(chat, "assistant", markdown),
        });
        await WaitAsync("document.querySelectorAll('.katex').length === 2 && !!document.querySelector('pre code') && document.fonts.status === 'loaded'",
            "wrapping fixture renders highlighted code and short table formulas");
        var sizes = new List<JsonElement>();
        foreach (int requestedWidth in new[] { 480, 980, 1600 })
        {
            _transcript.ClearSelection();
            _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(requestedWidth, 1000));
            await Task.Delay(220);
            var geometry = await EvalAsync<JsonElement>("__transcriptSmoke.resizeGeometry()");
            sizes.Add(geometry);
            _metrics["resize"] = sizes;
            double viewport = geometry.GetProperty("viewport").GetDouble();
            Check(Math.Abs(viewport - requestedWidth) < 30, "native viewport resized near requested width " + requestedWidth);
            Check(geometry.GetProperty("preScrollWidth").GetDouble() <= geometry.GetProperty("preClientWidth").GetDouble() + 1,
                "long highlighted code wraps without horizontal clipping at " + requestedWidth);
            Check(geometry.GetProperty("codeGlyphMaxRight").GetDouble() <= geometry.GetProperty("preRight").GetDouble() + 1,
                "every highlighted glyph stays inside its code surface at " + requestedWidth);
            Check(geometry.GetProperty("pageScrollWidth").GetDouble() <= geometry.GetProperty("pageClientWidth").GetDouble() + 1,
                "long Markdown URL and words do not widen the page at " + requestedWidth);
            Check(geometry.GetProperty("tableScrollWidth").GetDouble() <= geometry.GetProperty("tableClientWidth").GetDouble() + 1,
                "long table words wrap alongside short math at " + requestedWidth);
            Check(await EvalAsync<string>("document.querySelector('pre code').textContent") == sourceCode,
                "soft wrapping leaves code DOM source unchanged at " + requestedWidth);
            Check(await EvalAsync<string>("__transcriptSmoke.copyWholeCode()") == sourceCode,
                "Range copy has exactly the original code without soft line breaks at " + requestedWidth);
            if (requestedWidth is 480 or 1600)
            {
                _transcript.ClearSelection();
                await Task.Delay(50);
                await EvalAsync<bool>("(() => { scrollTo(0,0); return true; })()");
                await Task.Delay(80);
                string path = Path.Combine(Path.GetTempPath(), $"kynxa-transcript-wrap-{requestedWidth}.png");
                await CaptureViewportAsync(path);
                _metrics[requestedWidth == 480 ? "narrowPreview" : "widePreview"] = path;
            }
        }
        Check(sizes[2].GetProperty("messagesWidth").GetDouble() > 1400,
            "wide transcript is no longer capped at 1100 CSS pixels");
        Check(sizes[2].GetProperty("replyWidth").GetDouble() - sizes[0].GetProperty("replyWidth").GetDouble() > 900,
            "reply area expands with the available viewport");
        Check(sizes[0].GetProperty("preHeight").GetDouble() > sizes[2].GetProperty("preHeight").GetDouble() + 20,
            "narrow code gains visual lines instead of hiding horizontal content");
        _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 850));
        _transcript.ClearSelection();
        await Task.Delay(100);
    }

    private async Task CaptureViewportAsync(string path)
    {
        using var capture = new InMemoryRandomAccessStream();
        await _transcript.Browser.CoreWebView2.CapturePreviewAsync(CoreWebView2CapturePreviewImageFormat.Png, capture);
        capture.Seek(0);
        await using var file = File.Create(path);
        await capture.AsStreamForRead().CopyToAsync(file);
    }
}
