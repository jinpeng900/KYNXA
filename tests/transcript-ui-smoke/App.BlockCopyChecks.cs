using System.Text.Json;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Windows.ApplicationModel.DataTransfer;

namespace TranscriptUiSmoke;

public partial class App
{
    // Exercise shipped DOM, native clipboard acknowledgements and trusted keyboard input in an isolated fixture.
    // 在隔离测试中验证随包 DOM、原生剪贴板回执与可信键盘输入，不连接真实模型。
    private async Task CheckBlockCopyAsync()
    {
        string language = UiText.Language, palette = AppearanceService.Current.Id;
        var size = _window!.AppWindow.Size;
        try
        {
            // Keep this test-owned page active without taking the user's operating-system focus.
            // 仅模拟本测试页面处于活动状态，不抢占用户的系统窗口焦点。
            await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Emulation.setFocusEmulationEnabled", "{\"enabled\":true}");
            UiText.Initialize("zh-CN");
            _transcript.ClearSelection();
            await EvalAsync<bool>("""
                (() => {
                  window.__blockNativePost=chrome.webview.postMessage.bind(chrome.webview);
                  window.__blockRequests=[]; window.__blockReceipts=[]; window.__blockKeyLog=[];
                  window.__blockKeyListener=event => __blockKeyLog.push({type:event.type,key:event.key,code:event.code,
                    target:event.target?.className,isTrusted:event.isTrusted});
                  for(const type of ['keydown','keypress','keyup']) document.addEventListener(type,__blockKeyListener,true);
                  window.__blockReceiptListener=event => {
                    if(event.data?.type==='copyResult') __blockReceipts.push({...event.data,receivedAt:performance.now(),
                      states:[...document.querySelectorAll('.copy-block')].map(button=>button.dataset.copyState||'normal')});
                  };
                  chrome.webview.addEventListener('message',__blockReceiptListener);
                  chrome.webview.postMessage=value => {
                    if(value.type==='copy' && value.requestId?.startsWith('block-')) __blockRequests.push({...value,requestedAt:performance.now()});
                    __blockNativePost(value);
                  };
                  return true;
                })()
                """);
            var chat = Guid.NewGuid();
            const string code = "\tvar text = \"中文 🧪 < & >\";\n    return text;";
            string markdown = "BLOCK_COPY_BEFORE 正文\n\n```cs\n" + code + "\n```\n\n"
                + "```json\n{\n  \"姓名\": \"同学\",\n  \"active\": true\n}\n```\n\n"
                + "```\n通知模板 Template\n  保留缩进和换行。\n```\n\n"
                + "```plaintext\n\n第一段 Plain text\n\n第二段\n\n```\n\n"
                + "```unknown-custom\n<literal attr=\"value\"> & 中文\n```\n\n"
                + "```text\n```\n\n    INDENTED_CODE 原文\n\n"
                + MermaidFence("flowchart LR\n A[开始 Start] --> B[完成 Done]") + "\n\nBLOCK_COPY_AFTER 后文";
            var answer = Message(chat, "assistant", markdown);
            string original = JsonSerializer.Serialize(answer.Message);
            _transcript.ShowConversation(chat, [answer]);
            await WaitAsync("document.querySelectorAll('.content-block').length === 7 && document.querySelector('.mermaid-block')?.dataset.diagramState === 'ready'",
                "code, JSON, unlabeled/plain/unknown/empty/indented text each receives one independent copy control");
            await EvalAsync<bool>("(() => { window.__blockArticle=document.querySelector('article'); window.__blockFirst=document.querySelector('.content-block'); return true; })()");
            Check(await EvalAsync<bool>("[...document.querySelectorAll('.content-block')].every(block => block.querySelectorAll('.copy-block').length === 1 && block.querySelector('header[data-copy-ignore]') && block.querySelector('pre[data-code-complete=true] > code'))"),
                "each ordinary block has an ignored toolbar, a closed-source marker and exactly one copy button");
            Check(await EvalAsync<bool>("!document.querySelector('.mermaid-block .copy-block') && document.querySelectorAll('.mermaid-copy-source').length === 1"),
                "Mermaid keeps its existing source copy without a duplicate ordinary-block toolbar");
            Check(await EvalAsync<string>("document.querySelector('.content-block pre code').textContent") == code,
                "highlight spans preserve tabs, spaces, Unicode, angle brackets and ampersands exactly");
            for (int index = 0; index < 7; index++)
            {
                string source = await EvalAsync<string>($"document.querySelectorAll('.content-block')[{index}].querySelector('code').textContent");
                await CopyBlockAndCheckAsync(index, source);
                if (index == 0)
                    Check(await EvalAsync<bool>("document.querySelectorAll('.copy-block[data-copy-state=success]').length === 1 && !document.querySelector('.copy-message').dataset.copyState"),
                        "a successful block copy changes only its own icon, leaving other block and whole-message buttons untouched");
            }
            await WaitAsync("[...document.querySelectorAll('.copy-block')].every(button => !button.dataset.copyState)", "copy feedback returns to normal after its short acknowledgement period");
            Check(await EvalAsync<bool>("[...document.querySelectorAll('.content-block')].every(block => !!block.querySelector('.copy-block').title && !block.querySelector('.content-block-status').textContent)"),
                "local feedback clears and normal copy instructions return after acknowledgement expires");
            string selection = await EvalAsync<string>("(() => { const range=document.createRange(); range.selectNodeContents(__blockArticle.querySelector('.message-body')); getSelection().removeAllRanges(); getSelection().addRange(range); return __transcriptSmoke.copyEvent(); })()");
            Check(selection.Contains(code, StringComparison.Ordinal) && selection.Contains("通知模板 Template", StringComparison.Ordinal)
                && !selection.Contains("复制", StringComparison.Ordinal) && !selection.Contains("unknown-custom", StringComparison.Ordinal),
                "selection across multiple blocks excludes toolbar labels and copy controls while retaining original code and text");
            _transcript.ClearSelection();
            await CopyBlockMessageAsync(markdown);
            await CheckOtherLocalCopyFailuresAsync(chat);
            await CheckBlockCopyFailureAndNavigationAsync(chat, answer);
            await CheckBlockCopyKeyboardAsync();
            await CheckBlockCopyAppearanceAsync(answer, original, palette);
            await CheckBlockCopyStreamingAsync();
            _metrics["blockCopyScope"] = "native exact clipboard, isolated acknowledgement/reset, simulated failure/retry, keyboard, streaming/frozen DOM, cached navigation, language/palettes and 480/980 geometry";
        }
        finally
        {
            _metrics["blockCopyNativeRequests"] = await EvalAsync<JsonElement>("window.__blockRequests || []");
            _metrics["blockCopyNativeReceipts"] = await EvalAsync<JsonElement>("window.__blockReceipts || []");
            _metrics["blockCopyKeyLog"] = await EvalAsync<JsonElement>("window.__blockKeyLog || []");
            await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Emulation.setFocusEmulationEnabled", "{\"enabled\":false}");
            await EvalAsync<bool>("(() => { if(window.__blockNativePost) chrome.webview.postMessage=__blockNativePost; if(window.__blockReceiptListener) chrome.webview.removeEventListener('message',__blockReceiptListener); for(const type of ['keydown','keypress','keyup']) document.removeEventListener(type,__blockKeyListener,true); getSelection().removeAllRanges(); return true; })()");
            UiText.Initialize(language);
            AppearanceService.Apply(palette);
            _window.AppWindow.Resize(size);
        }
    }

    private async Task CopyBlockAndCheckAsync(int index, string source)
    {
        var request = await EvalAsync<JsonElement>($"(() => {{ const before=__blockRequests.length; document.querySelectorAll('.copy-block')[{index}].click(); return __blockRequests.length>before ? __blockRequests.at(-1) : null; }})()");
        await CheckBlockCopyReceiptAsync(index, source, request);
    }

    private async Task CheckBlockCopyReceiptAsync(int index, string source, JsonElement request)
    {
        _metrics["blockCopyLastRequest"] = new { index, sourceLength = source.Length, request };
        Check(request.ValueKind == JsonValueKind.Object, "block " + index + " emits a native copy request");
        string requestId = JsonSerializer.Serialize(request.GetProperty("requestId").GetString());
        await WaitAsync($"__blockReceipts.some(reply=>reply.requestId==={requestId})", "native host returns a result for block " + index);
        var receipt = await EvalAsync<JsonElement>($"__blockReceipts.find(reply=>reply.requestId==={requestId})");
        _metrics["blockCopyLastReceipt"] = new { index, receipt };
        Check(receipt.GetProperty("success").GetBoolean(), "native clipboard result succeeds for block " + index + ": " + receipt.GetRawText());
        Check(await EvalAsync<bool>($"document.querySelectorAll('.copy-block')[{index}]?.dataset.copyState === 'success'"), "matching native acknowledgement marks copied block " + index);
        Check(await EvalAsync<bool>($"(() => {{ const block=document.querySelectorAll('.content-block')[{index}], button=block.querySelector('.copy-block'), status=block.querySelector('.content-block-status'); return !button.hasAttribute('title') && !status.hasAttribute('title') && status.textContent==='已复制' && button.getAttribute('aria-label')==='已复制'; }})()"),
            "copy success stays beside its button without a floating tooltip");
        Check(await Clipboard.GetContent().GetTextAsync() == source, "native clipboard equals only block " + index + " source, without fence, language or toolbar");
        Check(await EvalAsync<bool>($"!!document.querySelectorAll('.copy-block')[{index}].querySelector('svg path') && !document.querySelectorAll('.copy-block')[{index}].querySelector('svg rect')"),
            "copied block " + index + " displays its checkmark");
    }

    private async Task CopyBlockMessageAsync(string markdown)
    {
        await EvalAsync<bool>("(() => { document.querySelector('.copy-message').click(); return true; })()");
        await WaitAsync("document.querySelector('.copy-message').dataset.copyState === 'success'", "whole-message copy still receives its native acknowledgement");
        Check(await Clipboard.GetContent().GetTextAsync() == markdown, "whole-message copy still preserves original Markdown and fences");
    }

    private async Task CheckOtherLocalCopyFailuresAsync(Guid chat)
    {
        // Failed receipts exercise local UI; no operating-system clipboard failure is forced.
        // 失败回执只验证局部提示，不强制造成操作系统剪贴板故障。
        foreach (var item in new[] { (Button: ".copy-message", Status: ".message-copy-status"), (Button: ".mermaid-copy-source", Status: ".mermaid-copy-status") })
        {
            var request = await EvalAsync<JsonElement>($$"""
                (() => {
                  const original=chrome.webview.postMessage; let captured;
                  chrome.webview.postMessage=request => { if(request.type==='copy') captured=request; else original.call(chrome.webview,request); };
                  try { document.querySelector('{{item.Button}}').click(); } finally { chrome.webview.postMessage=original; }
                  return captured;
                })()
                """);
            _transcript.Browser.CoreWebView2.PostWebMessageAsJson(JsonSerializer.Serialize(new { type = "copyResult", conversationId = chat, requestId = request.GetProperty("requestId").GetString(), success = false }));
            await WaitAsync($"document.querySelector('{item.Button}').dataset.copyState==='error'", "failed copy receipt reaches local " + item.Status);
            Check(await EvalAsync<bool>($"document.querySelector('{item.Status}').textContent==='复制失败，请重试。' && document.querySelector('{item.Button}').getAttribute('aria-label').includes('失败') && !document.querySelector('{item.Button}').hasAttribute('title')"),
                item.Status + " displays failure locally without a floating tooltip");
        }
    }

    private async Task CheckBlockCopyFailureAndNavigationAsync(Guid chat, ConversationMessageViewModel answer)
    {
        // Deliberately intercept one request to simulate a failed host result; this is not an OS clipboard-failure claim.
        // 主动截住一次请求来模拟宿主失败回执；此检查不声称制造或验证了操作系统剪贴板故障。
        await EvalAsync<bool>("(() => { window.__blockPost=chrome.webview.postMessage.bind(chrome.webview); chrome.webview.postMessage=value => { if(value.type==='copy') window.__blockRequest=value; else __blockPost(value); }; document.querySelector('.copy-block').click(); return true; })()");
        var request = await EvalAsync<JsonElement>("window.__blockRequest");
        Check(await EvalAsync<bool>("!document.querySelector('.copy-block').dataset.copyState"), "request alone cannot show a copy-success icon");
        _transcript.Browser.CoreWebView2.PostWebMessageAsJson(JsonSerializer.Serialize(new { type = "copyResult", conversationId = chat, requestId = request.GetProperty("requestId").GetString(), success = false }));
        await WaitAsync("document.querySelector('.copy-block').dataset.copyState === 'error'", "simulated failed host acknowledgement gives retry feedback");
        Check(await EvalAsync<bool>("document.querySelector('.content-block-status').textContent==='复制失败，请重试。' && document.querySelector('.copy-block').getAttribute('aria-label').includes('失败') && !document.querySelector('.copy-block').hasAttribute('title') && !document.querySelector('.content-block-status').hasAttribute('title') && !!document.querySelector('.copy-block svg rect')"),
            "failure stays beside the copy icon with an accessible retry hint and no floating tooltip");
        await EvalAsync<bool>("(() => { chrome.webview.postMessage=__blockPost; return true; })()");
        await CopyBlockAndCheckAsync(0, await EvalAsync<string>("document.querySelector('.content-block code').textContent"));
        await EvalAsync<bool>("(() => { chrome.webview.postMessage=value => { if(value.type==='copy') window.__blockRequest=value; else __blockPost(value); }; document.querySelector('.copy-block').click(); return true; })()");
        request = await EvalAsync<JsonElement>("window.__blockRequest");
        var otherChat = Guid.NewGuid();
        _transcript.ShowConversation(otherChat, [Message(otherChat, "assistant", "BLOCK_COPY_OTHER_CHAT")]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('BLOCK_COPY_OTHER_CHAT')", "navigation replaces the block-copy fixture with another conversation");
        _transcript.Browser.CoreWebView2.PostWebMessageAsJson(JsonSerializer.Serialize(new { type = "copyResult", conversationId = chat, requestId = request.GetProperty("requestId").GetString(), success = true }));
        await Task.Delay(100);
        Check(await EvalAsync<bool>("!document.querySelector('[data-copy-state=success]')"), "late previous-conversation acknowledgement changes no current copy control");
        await EvalAsync<bool>("(() => { chrome.webview.postMessage=__blockPost; return true; })()");
        _transcript.ShowConversation(chat, [answer]);
        await WaitAsync("document.querySelectorAll('.content-block').length === 7", "cached conversation restores its code and text blocks");
        Check(await EvalAsync<bool>("document.querySelector('article')===__blockArticle && document.querySelector('.content-block')===__blockFirst && document.querySelectorAll('.copy-block').length===7 && !document.querySelector('.copy-block').dataset.copyState"),
            "cache restoration reuses its original DOM, adds no duplicate controls and clears stale acknowledgement feedback");
        await CopyBlockAndCheckAsync(0, await EvalAsync<string>("document.querySelector('.content-block code').textContent"));
    }

    private async Task CheckBlockCopyKeyboardAsync()
    {
        _transcript.ClearSelection();
        _transcript.Browser.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
        await EvalAsync<bool>("(() => { document.querySelector('.copy-block').focus(); return true; })()");
        await BlockCopyKeyAsync("Tab", "Tab", 9);
        Check(await EvalAsync<bool>("document.activeElement===document.querySelectorAll('.copy-block')[1]"), "trusted Tab reaches the next visible block-copy button");
        await CheckBlockKeyboardCopyAsync(1, "Enter", "Enter", 13);
        await BlockCopyKeyAsync("Tab", "Tab", 9);
        Check(await EvalAsync<bool>("document.activeElement===document.querySelectorAll('.copy-block')[2]"), "trusted Tab continues to the next plain-text block");
        await CheckBlockKeyboardCopyAsync(2, " ", "Space", 32);
        Check(await EvalAsync<bool>("['keydown','keypress','keyup'].every(type=>__blockKeyLog.some(event=>event.type===type&&event.isTrusted&&event.target==='copy-block'&&event.key==='Enter'))"),
            "Enter follows trusted browser keydown, keypress and keyup default button activation");
        Check(await EvalAsync<bool>("['keydown','keypress','keyup'].every(type=>__blockKeyLog.some(event=>event.type===type&&event.isTrusted&&event.target==='copy-block'&&event.key===' '))"),
            "Space follows trusted browser keydown, keypress and keyup default button activation");
    }

    private async Task CheckBlockKeyboardCopyAsync(int index, string key, string code, int virtualKey)
    {
        int before = await EvalAsync<int>("__blockRequests.length");
        await BlockCopyKeyAsync(key, code, virtualKey);
        await WaitAsync($"__blockRequests.length>{before}", "trusted " + code + " emits a native block-copy request");
        var request = await EvalAsync<JsonElement>("__blockRequests.at(-1)");
        await CheckBlockCopyReceiptAsync(index, await EvalAsync<string>($"document.querySelectorAll('.content-block')[{index}].querySelector('code').textContent"), request);
    }

    private async Task BlockCopyKeyAsync(string key, string code, int virtualKey)
    {
        // CDP must carry the layout-generated character for Chromium's normal keypress/default activation.
        // CDP 须携带键盘布局生成的字符，Chromium 才能正常产生 keypress 并执行按钮默认激活。
        string text = key == "Enter" ? "\r" : key == " " ? " " : "";
        await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Input.dispatchKeyEvent", JsonSerializer.Serialize(new
        {
            type = text.Length > 0 ? "keyDown" : "rawKeyDown", key, code,
            windowsVirtualKeyCode = virtualKey, nativeVirtualKeyCode = virtualKey, text, unmodifiedText = text
        }));
        await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Input.dispatchKeyEvent", JsonSerializer.Serialize(new
        {
            type = "keyUp", key, code, windowsVirtualKeyCode = virtualKey, nativeVirtualKeyCode = virtualKey
        }));
    }

    private async Task CheckBlockCopyAppearanceAsync(ConversationMessageViewModel answer, string original, string paletteId)
    {
        await EvalAsync<bool>("(() => { const n=__transcriptSmoke.textNode(__blockArticle,'BLOCK_COPY_BEFORE'); getSelection().setBaseAndExtent(n,0,n,17); window.__blockSelection=transcriptSelectionText(); return true; })()");
        foreach (var palette in AppearanceService.Palettes)
        {
            AppearanceService.Apply(palette.Id);
            await WaitAsync($"getComputedStyle(document.documentElement).getPropertyValue('--appearance-accent').trim().toLowerCase()==='{palette.Accent.ToLowerInvariant()}'", "palette reaches block controls for " + palette.Id);
            Check(await EvalAsync<bool>("document.querySelector('article')===__blockArticle && document.querySelector('.content-block')===__blockFirst && transcriptSelectionText()===__blockSelection"),
                "palette update preserves message/block instances and active selection");
        }
        UiText.Initialize("en");
        await WaitAsync("document.documentElement.lang==='en' && /^(Copy|Copied)/.test(document.querySelector('.copy-block').getAttribute('aria-label'))", "block-copy accessible labels update live to English");
        Check(await EvalAsync<bool>("transcriptSelectionText()===__blockSelection && document.querySelector('.content-block')===__blockFirst"), "live language refresh preserves original block DOM and selected content");
        UiText.Initialize("zh-CN");
        AppearanceService.Apply(paletteId);
        await WaitAsync("document.documentElement.lang==='zh-CN'", "Chinese labels are restored before visual capture");
        _transcript.ClearSelection();
        var geometry = new List<JsonElement>();
        foreach (int width in new[] { 980, 480 })
        {
            _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(width, 850));
            await Task.Delay(180);
            await EvalAsync<bool>("(() => { scrollTo(0,0); return true; })()");
            var value = await EvalAsync<JsonElement>("""
                (() => ({ viewport:innerWidth,page:document.documentElement.scrollWidth,blocks:[...document.querySelectorAll('.content-block')].map(b=>{
                  const h=b.querySelector('header').getBoundingClientRect(), p=b.querySelector('pre').getBoundingClientRect(), c=b.querySelector('.copy-block').getBoundingClientRect(), r=b.getBoundingClientRect();
                  return {contained:c.left>=r.left&&c.right<=r.right&&c.top>=h.top&&c.bottom<=h.bottom,aboveCode:h.bottom<=p.top+1,width:r.width,buttonWidth:c.width};
                }) }))()
                """);
            geometry.Add(value);
            Check(value.GetProperty("page").GetDouble() <= value.GetProperty("viewport").GetDouble() + 1, "block toolbar creates no horizontal page overflow at width " + width);
            Check(value.GetProperty("blocks").EnumerateArray().All(block => block.GetProperty("contained").GetBoolean() && block.GetProperty("aboveCode").GetBoolean() && block.GetProperty("buttonWidth").GetDouble() >= 24),
                "all block copy buttons fit their toolbar without overlapping source at width " + width);
            await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), $"kynxa-transcript-block-copy-{width}.png"));
        }
        _metrics["blockCopyGeometry"] = geometry;
        Check(JsonSerializer.Serialize(answer.Message) == original, "copy, navigation, keyboard, palette, language and resize leave formal message JSON unchanged");
        UiText.Initialize("zh-CN");
    }

    private async Task CheckBlockCopyStreamingAsync()
    {
        var chat = Guid.NewGuid();
        const string first = "```text\nFIRST_CLOSED 源文\n```\n\n";
        var answer = Message(chat, "assistant", first + "```text\nVISIBLE_PART 中文", "streaming");
        _transcript.ShowConversation(chat, [answer]);
        await WaitAsync("document.querySelectorAll('.content-block').length===2 && document.querySelectorAll('.content-block pre')[1].dataset.codeComplete==='false'", "open streaming fence exposes current visible content as an incomplete block");
        await EvalAsync<bool>("(() => { window.__streamArticle=document.querySelector('article'); window.__streamClosed=document.querySelector('.content-block'); return true; })()");
        Check(await EvalAsync<bool>("document.querySelectorAll('.copy-block')[1].title.includes('当前')"), "an incomplete streaming block explicitly says copy current content");
        await CopyBlockAndCheckAsync(1, "VISIBLE_PART 中文");
        await EvalAsync<bool>("(() => { const n=__transcriptSmoke.textNode(__streamClosed,'FIRST_CLOSED'); getSelection().setBaseAndExtent(n,0,n,12); window.__frozenBlockSelection=transcriptSelectionText(); return true; })()");
        answer.Message.Content += "\nHIDDEN_PENDING_APPEND";
        answer.Refresh();
        await WaitAsync("transcriptState().pending", "selected text defers a newer streaming source update");
        // A trusted pointer click must preserve the reading selection as well as copy the visible snapshot.
        // 可信鼠标点击既要复制当前可见快照，也不能清除正在阅读的选区。
        var point = await EvalAsync<JsonElement>("""
            (() => {
              const button=document.querySelectorAll('.copy-block')[1], rect=button.getBoundingClientRect();
              window.__blockPointerTrusted=false;
              button.addEventListener('click',event=>window.__blockPointerTrusted=event.isTrusted,{once:true});
              return {x:rect.x+rect.width/2,y:rect.y+rect.height/2,before:__blockRequests.length,
                visible:rect.top>=0&&rect.bottom<=innerHeight&&rect.left>=0&&rect.right<=innerWidth};
            })()
            """);
        Check(point.GetProperty("visible").GetBoolean(), "pointer-copy target is inside the test WebView viewport");
        double x = point.GetProperty("x").GetDouble(), y = point.GetProperty("y").GetDouble();
        await DispatchMouse("mouseMoved", x, y);
        await DispatchMouse("mousePressed", x, y, "left", 1);
        await DispatchMouse("mouseReleased", x, y, "left");
        await WaitAsync($"__blockRequests.length>{point.GetProperty("before").GetInt32()}", "trusted pointer click sends a block-copy request");
        Check(await EvalAsync<bool>("__blockPointerTrusted"), "the block-copy click is a trusted browser pointer event");
        await CheckBlockCopyReceiptAsync(1, "VISIBLE_PART 中文", await EvalAsync<JsonElement>("__blockRequests.at(-1)"));
        Check(await EvalAsync<bool>("!document.querySelectorAll('.content-block code')[1].textContent.includes('HIDDEN_PENDING_APPEND') && transcriptSelectionText()===__frozenBlockSelection"),
            "block copy during selection freeze reads displayed DOM rather than newer hidden message text and preserves selection");
        _transcript.ClearSelection();
        await WaitAsync("document.querySelectorAll('.content-block code')[1].textContent.includes('HIDDEN_PENDING_APPEND')", "clearing selection applies the deferred streaming source");
        answer.Message.Content += "\n```";
        answer.Message.Status = "completed";
        answer.Refresh();
        await WaitAsync("document.querySelectorAll('.content-block pre')[1].dataset.codeComplete==='true' && !document.querySelectorAll('.copy-block')[1].title.includes('当前')", "closing and completing the fence updates its copy-current label to normal copy");
        Check(await EvalAsync<bool>("document.querySelector('article')===__streamArticle && document.querySelector('.content-block')===__streamClosed && document.querySelectorAll('.copy-block').length===2"),
            "stream append and completion preserve the earlier closed block without duplicated toolbars");
        await CopyBlockAndCheckAsync(1, "VISIBLE_PART 中文\nHIDDEN_PENDING_APPEND");
    }
}
