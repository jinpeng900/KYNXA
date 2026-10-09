using System.Text.Json;
using Microsoft.Web.WebView2.Core;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage.Streams;

namespace TranscriptUiSmoke;

public partial class App
{
    private async Task CheckOutputFormatAndResizeAsync()
    {
        var originalSize = _window!.AppWindow.Size;
        // Keep selection checks focused inside this fixture without changing the user's OS focus.
        // 只在夹具内模拟焦点以稳定选区检查，不改变用户的操作系统焦点。
        await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Emulation.setFocusEmulationEnabled", "{\"enabled\":true}");
        try
        {
            await CheckWrappingAndResizeAsync();
            await CheckOutputFormatsAsync();
        }
        finally
        {
            _transcript.ClearSelection();
            await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Emulation.setFocusEmulationEnabled", "{\"enabled\":false}");
            _window.AppWindow.Resize(originalSize);
        }
    }

    private async Task CheckWrappingAndResizeAsync()
    {
        string token = string.Concat(Enumerable.Repeat("LONG_TOKEN_0123456789_", 80));
        string sourceCode = "# 横向滚动不改变实际代码或复制结果\n"
            + "def configure_endpoint():\n"
            + "    endpoint = \"https://example.invalid/" + token + "\"\n"
            + "    payload = {\"label\": \"中文内容\", \"enabled\": True, \"retries\": 3}\n"
            + "    return endpoint, payload";
        string markdown = "## 代码横向滚动与回复宽度\n\n```python\n" + sourceCode + "\n```\n\n"
            + "普通 Markdown 长链接：https://example.invalid/" + token + "\n\n"
            + "连续长单词：" + token + "\n\n"
            + "| 类型 | 表达式 | 描述 |\n| --- | --- | --- |\n"
            + "| 速度 | $v=s/t$ | " + token + " |\n"
            + "| 平方 | $x^2$ | 宽度跟随聊天区，代码复制保留原文 |";
        var chat = Guid.NewGuid();
        _transcript.ShowConversation(chat, new[]
        {
            Message(chat, "user", "代码保留原始行并在框内横向滚动；普通长文本与表格自动换行。"),
            Message(chat, "assistant", markdown),
        });
        await WaitAsync("document.querySelectorAll('.katex').length === 2 && !!document.querySelector('pre code') && document.fonts.status === 'loaded'",
            "wrapping fixture renders highlighted code and short table formulas");
        await InstallFormatGeometryAsync();
        var sizes = new List<JsonElement>();
        foreach (int requestedWidth in new[] { 480, 980, 1600 })
        {
            _transcript.ClearSelection();
            _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(requestedWidth, 1000));
            await Task.Delay(220);
            await EvalAsync<bool>("(() => { const pre=document.querySelector('.content-block > pre'); pre.scrollLeft=0; pre.scrollIntoView({block:'center'}); return true; })()");
            await Task.Delay(100);
            var geometry = await EvalAsync<JsonElement>("__formatSmoke.geometry(document.querySelector('.content-block > pre'), 'LONG_TOKEN_', '\"')");
            sizes.Add(geometry);
            _metrics["resize"] = sizes;
            double viewport = geometry.GetProperty("viewport").GetDouble();
            Check(Math.Abs(viewport - requestedWidth) < 30, "native viewport resized near requested width " + requestedWidth);
            Check(geometry.GetProperty("preScrollWidth").GetDouble() > geometry.GetProperty("preClientWidth").GetDouble() + 100,
                "long highlighted code has horizontal overflow inside its own surface at " + requestedWidth);
            Check(geometry.GetProperty("codeWhiteSpace").GetString() == "pre"
                && geometry.GetProperty("codeOverflowWrap").GetString() == "normal"
                && geometry.GetProperty("codeWordBreak").GetString() == "normal",
                "code preserves original physical lines at " + requestedWidth);
            Check(Math.Abs(geometry.GetProperty("firstTop").GetDouble() - geometry.GetProperty("lastTop").GetDouble()) < 1,
                "the beginning and end of the long code line share one baseline at " + requestedWidth);
            Check(geometry.GetProperty("pageScrollWidth").GetDouble() <= geometry.GetProperty("pageClientWidth").GetDouble() + 1,
                "long Markdown URL and words do not widen the page at " + requestedWidth);
            Check(geometry.GetProperty("tableScrollWidth").GetDouble() <= geometry.GetProperty("tableClientWidth").GetDouble() + 1,
                "long table words wrap alongside short math at " + requestedWidth);
            Check(await EvalAsync<string>("document.querySelector('pre code').textContent") == sourceCode,
                "horizontal layout leaves code DOM source unchanged at " + requestedWidth);
            Check(await EvalAsync<string>("__transcriptSmoke.copyWholeCode()") == sourceCode,
                "Range copy has exactly the original code and indentation at " + requestedWidth);
            _transcript.ClearSelection();
            await CheckHorizontalWheelAsync("document.querySelector('.content-block > pre')", "LONG_TOKEN_", "\"", requestedWidth);
            Check(await EvalAsync<string>("__transcriptSmoke.copyWholeCode()") == sourceCode,
                "selected code copy remains exact after horizontal scrolling at " + requestedWidth);
            _transcript.ClearSelection();
            await ClickAndAwaitNativeCopyAsync("document.querySelector('.copy-block')", "scrolled code at " + requestedWidth);
            Check(await Clipboard.GetContent().GetTextAsync() == sourceCode,
                "native clipboard preserves original code after horizontal scrolling at " + requestedWidth);
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
        Check(Math.Abs(sizes[0].GetProperty("preHeight").GetDouble() - sizes[2].GetProperty("preHeight").GetDouble()) < 2,
            "narrow and wide code retain the same source-line height without soft wrapping");
        _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 850));
        _transcript.ClearSelection();
        await Task.Delay(100);
    }

    private async Task InstallFormatGeometryAsync()
    {
        await EvalAsync<bool>("""
            (() => {
              window.__formatSmoke = {
                glyph(code, offset) {
                  const walker=document.createTreeWalker(code,NodeFilter.SHOW_TEXT);
                  for(let node;(node=walker.nextNode());) {
                    if(offset<node.length) {
                      const range=document.createRange(); range.setStart(node,offset); range.setEnd(node,offset+1);
                      return range.getBoundingClientRect();
                    }
                    offset-=node.length;
                  }
                  throw new Error('Missing source glyph offset');
                },
                geometry(pre, startToken, endToken) {
                  const code=pre.querySelector('code'), source=code.textContent, first=source.indexOf(startToken);
                  const newline=source.indexOf('\n',first), end=newline<0?source.length:newline;
                  const last=source.lastIndexOf(endToken,end-1), firstRect=this.glyph(code,first), lastRect=this.glyph(code,last);
                  const rect=pre.getBoundingClientRect(), table=document.querySelector('.table-scroll'), style=getComputedStyle(pre);
                  return {
                    viewport:innerWidth, messagesWidth:document.getElementById('messages').getBoundingClientRect().width,
                    replyWidth:document.querySelector('[data-role=assistant] .message-body').getBoundingClientRect().width,
                    pageClientWidth:document.documentElement.clientWidth,pageScrollWidth:document.documentElement.scrollWidth,
                    pageScrollLeft:document.scrollingElement.scrollLeft,
                    preClientWidth:pre.clientWidth,preScrollWidth:pre.scrollWidth,preScrollLeft:pre.scrollLeft,
                    preHeight:rect.height,preLeft:rect.left,preRight:rect.right,
                    firstTop:firstRect.top,lastTop:lastRect.top,lastLeft:lastRect.left,lastRight:lastRect.right,
                    codeWhiteSpace:style.whiteSpace,codeOverflowWrap:style.overflowWrap,codeWordBreak:style.wordBreak,
                    tableClientWidth:table?.clientWidth||0,tableScrollWidth:table?.scrollWidth||0,
                    wheelX:Math.max(1,Math.min(innerWidth-2,rect.left+rect.width/2)),
                    wheelY:Math.max(1,Math.min(innerHeight-2,rect.top+rect.height/2))
                  };
                },
                section(number) {
                  const heading=[...document.querySelectorAll('.message-body h2')].find(node=>node.textContent.startsWith(number+'.'));
                  if(!heading) throw new Error('Missing format section '+number);
                  return heading.nextElementSibling;
                },
                copyContents(node) {
                  const range=document.createRange(); range.selectNodeContents(node);
                  getSelection().removeAllRanges(); getSelection().addRange(range);
                  return __transcriptSmoke.copyEvent();
                }
              };
              return true;
            })()
            """);
    }

    private async Task CheckHorizontalWheelAsync(string preExpression, string startToken, string endToken, int width)
    {
        string geometryExpression = $"__formatSmoke.geometry({preExpression},{JsonSerializer.Serialize(startToken)},{JsonSerializer.Serialize(endToken)})";
        await EvalAsync<bool>($"(() => {{ const pre={preExpression}; pre.scrollLeft=0; pre.scrollIntoView({{block:'center'}}); return true; }})()");
        await Task.Delay(80);
        var before = await EvalAsync<JsonElement>(geometryExpression);
        // Dispatch a browser wheel gesture rather than assigning the observed final scroll position.
        // 用浏览器滚轮手势验证滚动，不能直接赋值最终 scrollLeft 冒充交互通过。
        await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Input.dispatchMouseEvent", JsonSerializer.Serialize(new
        {
            type = "mouseWheel",
            x = before.GetProperty("wheelX").GetDouble(),
            y = before.GetProperty("wheelY").GetDouble(),
            deltaX = before.GetProperty("preScrollWidth").GetDouble() * 2,
            deltaY = 0,
        }));
        await WaitAsync($"(() => {{ const pre={preExpression}; return pre.scrollLeft>0 && pre.scrollLeft>=pre.scrollWidth-pre.clientWidth-2; }})()",
            "horizontal wheel reaches the code block's far end at " + width);
        var after = await EvalAsync<JsonElement>(geometryExpression);
        _metrics["horizontalWheel" + width + "-" + startToken] = new { before, after };
        Check(after.GetProperty("lastLeft").GetDouble() >= after.GetProperty("preLeft").GetDouble()
            && after.GetProperty("lastRight").GetDouble() <= after.GetProperty("preRight").GetDouble() + 1,
            "the last source glyph is reachable and visible inside the scrolled block at " + width);
        Check(Math.Abs(after.GetProperty("firstTop").GetDouble() - after.GetProperty("lastTop").GetDouble()) < 1,
            "horizontal scrolling does not introduce visual line breaks at " + width);
        Check(Math.Abs(after.GetProperty("pageScrollLeft").GetDouble()) < 1
            && after.GetProperty("pageScrollWidth").GetDouble() <= after.GetProperty("pageClientWidth").GetDouble() + 1,
            "horizontal code scrolling never widens or shifts the page at " + width);
    }

    private async Task CheckOutputFormatsAsync()
    {
        const string shell = "#!/bin/bash\n# 中文注释：安全演示，不执行命令\nfor item in alpha beta; do\n  if [ -n \"$item\" ]; then\n    printf '%s\\n' \"$item\"\n  fi\ndone";
        string longLine = "横向滚动_START_" + string.Concat(Enumerable.Repeat("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!@#$%^&*()_+-=[]{}|", 48)) + "_END";
        const string plain = "这是普通说明文字，包含 English words。\n第二行保持中文与 English 的换行。\nThird line: 原样显示。";
        const string quote = "第一行引用内容。\n第二行仍属于同一个引用块。\n第三行包含中文与 English。";
        const string special = "中英文 Mixed text 特殊符号：!@#$%^&*()_+-=[]{}|";
        const string boundary = "第一行内容\n\n\n\n第二行前面有三个空行\n\n\n第三行内容\n\n\n\n\n结束行";
        string markdown = "## 5. Shell脚本示例\n\n```shell\n" + shell + "\n```\n\n"
            + "## 6. 空代码块\n\n```txt\n```\n\n"
            + "## 7. 超长单行文本代码块\n\n```txt\n" + longLine + "\n```\n\n"
            + "## 8. 纯普通文本段落\n\n" + plain + "\n\n"
            + "## 9. 引用块文本\n\n> " + quote.Replace("\n", "\n> ") + "\n\n"
            + "## 10. 混合中英文纯文本\n\n" + special + "\n\n"
            + "## 11. 代码块边界测试\n\n```txt\n" + boundary + "\n```";
        var chat = Guid.NewGuid();
        _transcript.ClearSelection();
        _transcript.ShowConversation(chat, [Message(chat, "assistant", markdown)]);
        await WaitAsync("document.querySelectorAll('.message-body h2').length===7 && document.querySelectorAll('.content-block').length===4 && document.fonts.status==='loaded'",
            "sections 5 through 11 render with exactly four fenced content blocks");
        await InstallFormatGeometryAsync();
        Check(await EvalAsync<bool>("__formatSmoke.section(5).matches('.content-block') && __formatSmoke.section(5).querySelector('code').dataset.language==='shell'"),
            "multiline shell source remains a labelled fenced block");
        Check(await EvalAsync<bool>("__formatSmoke.section(6).matches('.content-block') && !!__formatSmoke.section(6).querySelector('.copy-block') && __formatSmoke.section(6).querySelector('code').textContent===''"),
            "the empty txt block retains its header and copy button with truly empty source");
        Check(await EvalAsync<bool>("__formatSmoke.section(7).querySelector('code').dataset.language==='txt' && !__formatSmoke.section(7).querySelector('code').textContent.includes('\\n')"),
            "the long txt example contains exactly one physical source line");
        Check(await EvalAsync<bool>("__formatSmoke.section(8).matches('p') && !__formatSmoke.section(8).querySelector('code,pre,.content-block') && __formatSmoke.section(8).querySelectorAll('br').length===2"),
            "ordinary Chinese and English stay unboxed with their two original line breaks");
        Check(await EvalAsync<string>("__formatSmoke.copyContents(__formatSmoke.section(8))") == plain,
            "ordinary paragraph selected copy preserves both languages and exact newlines");
        _transcript.ClearSelection();
        Check(await EvalAsync<bool>("__formatSmoke.section(9).matches('blockquote') && !__formatSmoke.section(9).querySelector('pre,code,.copy-block') && __formatSmoke.section(9).querySelectorAll('br').length===2"),
            "the multiline quote keeps its quote semantics and gains no code copy toolbar");
        Check(await EvalAsync<string>("__formatSmoke.copyContents(__formatSmoke.section(9).querySelector('p'))") == quote,
            "selected quote text preserves its three original lines");
        _transcript.ClearSelection();
        Check(await EvalAsync<bool>("__formatSmoke.section(10).matches('p') && !__formatSmoke.section(10).querySelector('code,pre,.content-block')"),
            "mixed-language special-symbol text remains an ordinary paragraph");
        Check(await EvalAsync<string>("__formatSmoke.section(10).textContent") == special,
            "every requested special symbol survives rendering exactly");
        Check(await EvalAsync<string>("__formatSmoke.copyContents(__formatSmoke.section(10))") == special,
            "every special symbol survives selected copying exactly");
        _transcript.ClearSelection();
        foreach (var example in new[] { (Section: 5, Source: shell), (Section: 6, Source: ""), (Section: 7, Source: longLine), (Section: 11, Source: boundary) })
        {
            Check(await EvalAsync<string>($"__formatSmoke.section({example.Section}).querySelector('code').textContent") == example.Source,
                "section " + example.Section + " preserves exact fenced source including all blank lines");
            if (example.Source.Length > 0)
            {
                Check(await EvalAsync<string>($"__formatSmoke.copyContents(__formatSmoke.section({example.Section}).querySelector('code'))") == example.Source,
                    "section " + example.Section + " selected copying preserves exact source whitespace");
                _transcript.ClearSelection();
            }
            await ClickAndAwaitNativeCopyAsync($"__formatSmoke.section({example.Section}).querySelector('.copy-block')", "format section " + example.Section);
            Check(await Clipboard.GetContent().GetTextAsync() == example.Source,
                "section " + example.Section + " native clipboard contains only its exact source");
        }
        _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 1000));
        await Task.Delay(220);
        await CheckHorizontalWheelAsync("__formatSmoke.section(7).querySelector('pre')", "横向滚动_START_", "D", 980);
        Check(await EvalAsync<string>("__formatSmoke.section(7).querySelector('code').textContent") == longLine,
            "scrolling the user's one-line txt fixture leaves its source unchanged");
        Check(await EvalAsync<string>("__formatSmoke.copyContents(__formatSmoke.section(7).querySelector('code'))") == longLine,
            "scrolled long txt selected copy contains no soft-wrap newlines");
        _transcript.ClearSelection();
        await EvalAsync<bool>("(() => { __formatSmoke.section(7).scrollIntoView({block:'start'}); return true; })()");
        await Task.Delay(80);
        string preview = Path.Combine(Path.GetTempPath(), "kynxa-transcript-output-format.png");
        await CaptureViewportAsync(preview);
        _metrics["outputFormatPreview"] = preview;
        _metrics["outputFormat"] = await EvalAsync<JsonElement>("({sections:[...document.querySelectorAll('.message-body h2')].map(node=>node.textContent),blocks:[...document.querySelectorAll('.content-block')].map(block=>({language:block.querySelector('code').dataset.language,length:block.querySelector('code').textContent.length,newlines:(block.querySelector('code').textContent.match(/\\n/g)||[]).length})),ordinaryBreaks:__formatSmoke.section(8).querySelectorAll('br').length,quoteBreaks:__formatSmoke.section(9).querySelectorAll('br').length})");
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 850));
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
