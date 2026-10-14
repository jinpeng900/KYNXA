using System.Text.Json;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Windows.ApplicationModel.DataTransfer;

namespace TranscriptUiSmoke;

public partial class App
{
    // Use the production control and native clipboard receipts with isolated, synthetic messages.
    // 使用正式控件与原生剪贴板回执，数据仅来自隔离的合成消息。
    private async Task CheckContentReadingAsync()
    {
        string language = UiText.Language, palette = AppearanceService.Current.Id;
        var size = _window!.AppWindow.Size;
        try
        {
            await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Emulation.setFocusEmulationEnabled", "{\"enabled\":true}");
            _transcript.Browser.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
            UiText.Initialize("zh-CN");
            _transcript.ClearSelection();
            await EvalAsync<bool>("""
                (() => {
                  window.__readingNativePost=chrome.webview.postMessage.bind(chrome.webview);
                  window.__readingRequests=[]; window.__readingReceipts=[]; window.__readingGlobalFeedback=[];
                  window.__readingReceiptListener=event => {
                    if(event.data?.type==='copyResult') __readingReceipts.push(event.data);
                  };
                  chrome.webview.addEventListener('message',__readingReceiptListener);
                  chrome.webview.postMessage=request => {
                    if(request.type==='copy') __readingRequests.push(request);
                    if(/copy.*feedback|copied|copyFailed/i.test(request.type||'')) __readingGlobalFeedback.push(request);
                    __readingNativePost(request);
                  };
                  return true;
                })()
                """);
            await CheckConversationReadingPositionAsync();
            await CheckBlankLineIntegrityAsync();
            await CheckTableReadingAsync();
            await CheckLongContentReadingAsync();
            Check(await EvalAsync<int>("__readingGlobalFeedback.length") == 0,
                "table/block copy emits no page-wide success or failure feedback request");
            _metrics["contentReadingScope"] = "real WinUI/WebView2; native clipboard and host receipts; simulated failed receipts; safe DOM-only table boundary fixtures; synthetic LF/CRLF streaming; 480/980/1600/2400 geometry, localization, cached reading anchors and selection";
        }
        finally
        {
            _metrics["contentReadingRequests"] = await EvalAsync<JsonElement>("window.__readingRequests || []");
            _metrics["contentReadingReceipts"] = await EvalAsync<JsonElement>("window.__readingReceipts || []");
            await EvalAsync<bool>("(() => { if(window.__readingNativePost) chrome.webview.postMessage=__readingNativePost; if(window.__readingReceiptListener) chrome.webview.removeEventListener('message',__readingReceiptListener); getSelection().removeAllRanges(); return true; })()");
            await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Emulation.setFocusEmulationEnabled", "{\"enabled\":false}");
            UiText.Initialize(language);
            AppearanceService.Apply(palette);
            _window.AppWindow.Resize(size);
        }
    }

    private async Task CheckBlankLineIntegrityAsync()
    {
        // Different source lengths must remain different; presentation must never invent a missing blank line.
        // 不同源码行数必须保持区别；展示层不能为模型少写的空行擅自补造内容。
        const string tenLines = "第一行\n\n\n第二行\n\n\n\n第三行\n\n结束_END";
        const string nineLines = "第一行\n\n\n第二行\n\n\n第三行\n\n结束_END";
        var blankLineMetrics = new List<JsonElement>();
        foreach (var fixture in new[]
        {
            (Name: "LF10", Original: tenLines, Displayed: tenLines, Lines: 10),
            (Name: "LF9", Original: nineLines, Displayed: nineLines, Lines: 9),
            (Name: "CRLF10", Original: tenLines.Replace("\n", "\r\n"), Displayed: tenLines, Lines: 10)
        })
        {
            var chat = Guid.NewGuid();
            string newline = fixture.Name == "CRLF10" ? "\r\n" : "\n";
            string markdown = "BLANK_INTEGRITY_" + fixture.Name + newline + newline + "```txt" + newline
                + fixture.Original + newline + "```";
            var answer = Message(chat, "assistant", markdown);
            _transcript.ShowConversation(chat, [answer]);
            await WaitAsync("__transcriptSmoke.bodyText().includes(" + JsonSerializer.Serialize("BLANK_INTEGRITY_" + fixture.Name)
                + ") && document.querySelector('.content-block pre')?.dataset.codeComplete==='true'", fixture.Name + " blank-line fixture reaches the completed production code renderer");
            Check(await EvalAsync<string>("document.querySelector('.content-block code').textContent") == fixture.Displayed,
                fixture.Name + " renderer preserves each empty source line, including the gap between the second and third labels");
            Check(await EvalAsync<string>("document.querySelector('.content-block-lines').dataset.lineCount") == fixture.Lines.ToString(),
                fixture.Name + " visible source-line counter agrees with the provided blank-line fixture");
            Check(await EvalAsync<string>("(() => { const range=document.createRange();range.selectNodeContents(document.querySelector('.content-block code'));getSelection().removeAllRanges();getSelection().addRange(range);return __transcriptSmoke.copyEvent(); })()") == fixture.Displayed,
                fixture.Name + " selected-source copy preserves all consecutive empty lines");
            _transcript.ClearSelection();
            await CopyReadingAndCheckAsync("document.querySelector('.copy-block')", fixture.Displayed, ".copy-block", ".content-block-status");
            await CopyReadingAndCheckAsync("document.querySelector('.copy-message')", markdown, ".copy-message", ".message-copy-status");
            Check(answer.Message.Content == markdown, fixture.Name + " display and clipboard actions leave formal LF/CRLF source unchanged");
            blankLineMetrics.Add(await EvalAsync<JsonElement>("({label:document.querySelector('.message-body p').textContent,lines:Number(document.querySelector('.content-block-lines').dataset.lineCount),source:document.querySelector('.content-block code').textContent})"));
        }
        _metrics["contentBlankLineIntegrity"] = blankLineMetrics;

        string streamingMarkdown = "BLANK_STREAM_CRLF\r\n\r\n```txt\r\n" + tenLines.Replace("\n", "\r\n") + "\r\n```\r\n\r\nBLANK_STREAM_END";
        int firstBoundary = streamingMarkdown.IndexOf("第二行", StringComparison.Ordinal) + "第二行".Length + 1;
        int secondBoundary = streamingMarkdown.IndexOf("第三行", StringComparison.Ordinal) - 1;
        var streamChat = Guid.NewGuid();
        var streamingAnswer = Message(streamChat, "assistant", streamingMarkdown[..firstBoundary], "streaming");
        _transcript.ShowConversation(streamChat, [streamingAnswer]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('BLANK_STREAM_CRLF') && document.querySelector('.content-block code')?.textContent.includes('第二行')===true && document.querySelector('.content-block pre').dataset.codeComplete==='false'",
            "streaming blank-line fixture renders a CR whose LF has not arrived yet");
        streamingAnswer.Message.Content += streamingMarkdown[firstBoundary..secondBoundary];
        streamingAnswer.Refresh();
        await Task.Delay(150);
        Check(streamingAnswer.Message.Content.EndsWith("\r", StringComparison.Ordinal), "next synthetic streaming chunk ends inside another CRLF boundary");
        streamingAnswer.Message.Content += streamingMarkdown[secondBoundary..];
        streamingAnswer.Message.Status = "completed";
        streamingAnswer.Refresh();
        await WaitAsync("__transcriptSmoke.bodyText().includes('BLANK_STREAM_END') && document.querySelector('.content-block pre')?.dataset.codeComplete==='true'", "closing a CRLF stream applies its complete blank-line source");
        Check(await EvalAsync<string>("document.querySelector('.content-block code').textContent") == tenLines
            && await EvalAsync<string>("document.querySelector('.content-block-lines').dataset.lineCount") == "10",
            "CRLF split across streaming chunks retains all ten lines and the three blank lines between labels");
        await CopyReadingAndCheckAsync("document.querySelector('.copy-block')", tenLines, ".copy-block", ".content-block-status");
        await CopyReadingAndCheckAsync("document.querySelector('.copy-message')", streamingMarkdown, ".copy-message", ".message-copy-status");
        Check(streamingAnswer.Message.Content == streamingMarkdown, "stream completion and copies preserve the exact raw CRLF message");
    }

    private async Task CheckConversationReadingPositionAsync()
    {
        // Navigate through the public host API so its default openAtBottom=true participates in the regression.
        // 通过公开宿主 API 切换，让默认 openAtBottom=true 也参与验证，避免只测浏览器内部缓存。
        _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 850));
        var chatA = Guid.NewGuid();
        var chatB = Guid.NewGuid();
        var chatC = Guid.NewGuid();
        string source = string.Join("\n", Enumerable.Range(1, 45).Select(index => $"READ_SOURCE_{index:000} 中文English " + new string('X', 125)));
        string markdownA = "READ_CACHE_A\n\n" + string.Join("\n\n", Enumerable.Range(1, 20).Select(index => $"A_BEFORE_{index:000} 阅读前文"))
            + "\n\n```txt\n" + source + "\n```\n\n" + string.Join("\n\n", Enumerable.Range(1, 70).Select(index => $"A_AFTER_{index:000} 阅读后文"));
        var answerA = Message(chatA, "assistant", markdownA);
        var answerB = Message(chatB, "assistant", "READ_CACHE_B 短会话");
        var answerC = Message(chatC, "assistant", "READ_CACHE_C\n\n" + string.Join("\n\n", Enumerable.Range(1, 50).Select(index => $"C_{index:000} 第三个会话")));
        _transcript.ShowConversation(chatA, [answerA]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('READ_CACHE_A') && __transcriptSmoke.bottomDistance()<4 && transcriptState().following", "first visit to an uncached long conversation still opens at its latest message");
        await EvalAsync<bool>("""
            (() => {
              window.__readingReturnBlock=document.querySelector('.content-block');
              __transcriptSmoke.scroller().dispatchEvent(new WheelEvent('wheel',{deltaY:-120,bubbles:true}));
              __readingReturnBlock.scrollIntoView({block:'start'});scrollBy(0,-90);
              __readingReturnBlock.querySelector('[data-block-action=wrap]').click();
              __readingReturnBlock.querySelector('[data-block-action=fold]').click();
              return true;
            })()
            """);
        await WaitAsync("__readingReturnBlock.dataset.wrap==='true' && __readingReturnBlock.dataset.collapsed==='true' && !transcriptState().following && __transcriptSmoke.bottomDistance()>300", "reader wraps and folds content while remaining above the latest message");
        await Task.Delay(100);
        await EvalAsync<bool>("(() => { window.__readingReturnView={top:scrollY,blockTop:__readingReturnBlock.getBoundingClientRect().top};return true; })()");
        _transcript.ShowConversation(chatB, [answerB]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('READ_CACHE_B')", "short conversation B replaces the cached reading surface A");
        _transcript.ShowConversation(chatA, [answerA]);
        await WaitAsync("document.querySelector('.content-block')===__readingReturnBlock && __transcriptSmoke.bodyText().includes('READ_CACHE_A')", "returning A reuses the original wrapped and folded content DOM");
        await Task.Delay(220);
        var returned = await EvalAsync<JsonElement>("({top:scrollY,expectedTop:__readingReturnView.top,blockTop:__readingReturnBlock.getBoundingClientRect().top,expectedBlockTop:__readingReturnView.blockTop,bottomDistance:__transcriptSmoke.bottomDistance(),following:transcriptState().following,wrap:__readingReturnBlock.dataset.wrap,collapsed:__readingReturnBlock.dataset.collapsed})");
        _metrics["contentConversationReadingReturn"] = returned;
        Check(Math.Abs(returned.GetProperty("top").GetDouble() - returned.GetProperty("expectedTop").GetDouble()) < 2
            && Math.Abs(returned.GetProperty("blockTop").GetDouble() - returned.GetProperty("expectedBlockTop").GetDouble()) < 2
            && returned.GetProperty("bottomDistance").GetDouble() > 300 && !returned.GetProperty("following").GetBoolean(),
            "A to B to A restores the previous reading position instead of obeying the first automatic host openAtBottom request");
        Check(returned.GetProperty("wrap").GetString() == "true" && returned.GetProperty("collapsed").GetString() == "true",
            "viewport restoration preserves the cached wrap and fold preferences");
        answerA.Message.Content += "\n\nA_DELAYED_REFRESH 到达的后文";
        answerA.Refresh();
        await WaitAsync("__transcriptSmoke.bodyText().includes('A_DELAYED_REFRESH')", "late host refresh arrives after cached position restoration");
        await EvalAsync<bool>("(() => { window.__readingReturnBlock=document.querySelector('.content-block');document.fonts.ready.then(()=>dispatchEvent(new Event('resize')));document.querySelector('.message-body').lastElementChild.style.paddingBottom='80px';return true; })()");
        await Task.Delay(220);
        Check(await EvalAsync<bool>("!transcriptState().following && __transcriptSmoke.bottomDistance()>300 && Math.abs(__readingReturnBlock.getBoundingClientRect().top-__readingReturnView.blockTop)<2"),
            "delayed snapshots, font readiness and a real ResizeObserver notification cannot pull restored reading to the bottom");
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(960, 850));
        await Task.Delay(180);
        Check(await EvalAsync<bool>("!transcriptState().following && __transcriptSmoke.bottomDistance()>300 && __readingReturnBlock.getBoundingClientRect().top<innerHeight"),
            "native width changes after a cached revisit keep the reader near their content instead of resuming following");
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 850));
        await Task.Delay(100);
        _transcript.JumpToLatest();
        await WaitAsync("__transcriptSmoke.bottomDistance()<4 && transcriptState().following", "explicit host JumpToLatest remains available after reading-position restoration");
        _transcript.ShowConversation(chatB, [answerB]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('READ_CACHE_B')", "bottom-following A is cached while visiting B again");
        _transcript.ShowConversation(chatA, [answerA]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('READ_CACHE_A') && __transcriptSmoke.bottomDistance()<4 && transcriptState().following", "returning to an A that was already at bottom keeps the latest-message follow state");
        answerA.Message.Content += "\n\nA_FOLLOWING_APPEND 底部追加内容";
        answerA.Refresh();
        await WaitAsync("__transcriptSmoke.bodyText().includes('A_FOLLOWING_APPEND') && __transcriptSmoke.bottomDistance()<4 && transcriptState().following", "a later append follows only when A was already following its latest content");

        await EvalAsync<bool>("(() => { __transcriptSmoke.scroller().dispatchEvent(new WheelEvent('wheel',{deltaY:-120,bubbles:true}));const anchor=[...document.querySelectorAll('.message-body > p')].find(node=>node.textContent.startsWith('A_AFTER_020'));anchor.scrollIntoView({block:'start'});scrollBy(0,-90);window.__readingRapidAnchor=anchor;window.__readingRapidAnchorTop=anchor.getBoundingClientRect().top;return true; })()");
        await WaitAsync("!transcriptState().following && __transcriptSmoke.bottomDistance()>300", "reader pauses again at a middle paragraph before rapid navigation");
        _transcript.ShowConversation(chatB, [answerB]);
        _transcript.ShowConversation(chatC, [answerC]);
        _transcript.ShowConversation(chatA, [answerA]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('READ_CACHE_A') && !__transcriptSmoke.bodyText().includes('READ_CACHE_C')", "rapid A to B to C to A finishes on the latest requested conversation");
        await Task.Delay(320);
        Check(await EvalAsync<bool>("__readingRapidAnchor.isConnected && Math.abs(__readingRapidAnchor.getBoundingClientRect().top-__readingRapidAnchorTop)<2 && !transcriptState().following && __transcriptSmoke.bottomDistance()>300"),
            "late frames from rapid conversation switches cannot replace A's reading anchor or jump to its bottom");
        _transcript.ShowConversation(chatA, [answerA], openAtBottom: true);
        await WaitAsync("__transcriptSmoke.bottomDistance()<4 && transcriptState().following", "a subsequent explicit same-conversation openAtBottom request still navigates to latest");
    }

    private async Task CheckTableReadingAsync()
    {
        var chat = Guid.NewGuid();
        const string markdown = "TABLE_BEFORE 表格正文\n\n| 名称 Name | 说明 | 公式 |\n| --- | --- | --- |\n| 中文 🧪 | A\\|B | $v=s/t$ |\n| 空值 | | =SUM(1,2) |\n\nTABLE_AFTER 表格后文\n\n| 仅表头 | Empty |\n| --- | --- |";
        var answer = Message(chat, "assistant", markdown);
        string original = JsonSerializer.Serialize(answer.Message);
        _transcript.ShowConversation(chat, [answer]);
        await WaitAsync("document.querySelectorAll('.table-block').length===2 && !!document.querySelector('.katex')",
            "Markdown tables receive independent controls while their formula uses the production renderer");
        Check(await EvalAsync<bool>("[...document.querySelectorAll('.table-block')].every(block=>block.querySelectorAll('.copy-table').length===1 && block.querySelector('.table-block-header[data-copy-ignore]') && block.querySelector('.table-scroll > table'))"),
            "one table control group is outside its horizontal scroll surface and excluded from copied selections");
        await CopyReadingAndCheckAsync("document.querySelector('.copy-table')", "名称 Name\t说明\t公式\r\n中文 🧪\tA|B\t$v=s/t$\r\n空值\t\t=SUM(1,2)", ".copy-table", ".table-block-status");
        Check(await EvalAsync<bool>("!document.querySelector('.copy-message').dataset.copyState && !document.querySelectorAll('.copy-table')[1].dataset.copyState"),
            "successful table copy updates only the selected table, without whole-message or neighbouring-table feedback");
        await EvalAsync<bool>("(() => { document.querySelector('.table-copy-options').click(); return true; })()");
        await WaitAsync("document.querySelector('.table-copy-options').getAttribute('aria-expanded')==='true' && !document.querySelector('.table-copy-menu').hidden", "table copy formats are available in the accessible local menu");
        await CopyReadingAndCheckAsync("document.querySelector('.copy-table-markdown')", "| 名称 Name | 说明 | 公式 |\n| --- | --- | --- |\n| 中文 🧪 | A\\|B | $v=s/t$ |\n| 空值 |  | =SUM(1,2) |", ".copy-table", ".table-block-status");
        Check(await EvalAsync<bool>("document.querySelector('.table-copy-options').getAttribute('aria-expanded')==='false'"),
            "selecting a copy format closes the menu");
        await CopyReadingAndCheckAsync("document.querySelectorAll('.copy-table')[1]", "仅表头\tEmpty", ".copy-table", null, checkLocal: false);
        string selected = await EvalAsync<string>("(() => { const range=document.createRange(); range.selectNodeContents(document.querySelector('.message-body')); getSelection().removeAllRanges(); getSelection().addRange(range); return __transcriptSmoke.copyEvent(); })()");
        Check(selected.Contains("中文 🧪", StringComparison.Ordinal) && selected.Contains("$v=s/t$", StringComparison.Ordinal)
            && !selected.Contains("复制表格", StringComparison.Ordinal) && !selected.Contains("Markdown", StringComparison.Ordinal),
            "cross-table selection retains cell text and TeX without toolbar controls or local copy feedback");
        _transcript.ClearSelection();
        await CopyReadingAndCheckAsync("document.querySelector('.copy-message')", markdown, ".copy-message", ".message-copy-status");
        await CheckTableBoundaryCellsAsync();
        await CheckTableFailureAndNavigationAsync(chat, answer);
        await CheckTableGeometryAndLanguageAsync();
        Check(JsonSerializer.Serialize(answer.Message) == original, "table actions, DOM edge fixtures and navigation leave formal message JSON unchanged");
    }

    private async Task<JsonElement> CopyReadingAndCheckAsync(string buttonExpression, string expected, string stateSelector, string? statusSelector, bool checkLocal = true)
    {
        int before = await EvalAsync<int>("__readingRequests.length");
        await EvalAsync<bool>($"(() => {{ {buttonExpression}.click(); return true; }})()");
        await WaitAsync($"__readingRequests.length>{before}", "copy action emits a native clipboard request");
        var request = await EvalAsync<JsonElement>("__readingRequests.at(-1)");
        Check(request.GetProperty("text").GetString() == expected, "copy request contains exactly the requested table/block format");
        string id = JsonSerializer.Serialize(request.GetProperty("requestId").GetString());
        await WaitAsync($"__readingReceipts.some(receipt=>receipt.requestId==={id} && receipt.success===true)", "native clipboard acknowledges the current copy request");
        Check(await Clipboard.GetContent().GetTextAsync() == expected, "Windows clipboard contains exact Unicode, whitespace, TeX and source delimiters");
        if (checkLocal)
        {
            string selector = JsonSerializer.Serialize(stateSelector);
            await WaitAsync($"document.querySelector({selector})?.dataset.copyState==='success'", "matching receipt displays a local success checkmark");
            Check(await EvalAsync<bool>($"!document.querySelector({selector}).hasAttribute('title') && !!document.querySelector({selector}).querySelector('svg path') && !document.querySelector({selector}).querySelector('svg rect')"),
                "local success has a checkmark and no duplicate floating title tooltip");
            if (statusSelector is not null)
                Check(await EvalAsync<string>($"document.querySelector({JsonSerializer.Serialize(statusSelector)}).textContent") == "已复制", "successful copy feedback is written beside the current button");
        }
        return request;
    }

    private async Task CheckTableBoundaryCellsAsync()
    {
        // The Markdown renderer intentionally rejects HTML. Safe DOM construction covers serializer edges
        // without claiming that model-supplied HTML or multiline pipe cells are accepted as markup.
        // Markdown 有意禁止 HTML；安全 DOM 构造仅覆盖序列化边界，不代表允许模型 HTML 或多行管道单元格。
        await EvalAsync<bool>("""
            (() => {
              const table=document.querySelector('.table-block table');
              table.tBodies[0].rows[0].cells[1].replaceChildren(document.createTextNode('第一行'),document.createElement('br'),document.createTextNode('第二行 "引号" &lt; &#65;'));
              const empty=document.createElement('table'), scroll=document.createElement('div');
              scroll.className='table-scroll'; scroll.append(empty); document.querySelector('.message-body').append(scroll);
              window.kynxaTables.hydrate(document.querySelector('.message-body'));
              return true;
            })()
            """);
        await CopyReadingAndCheckAsync("document.querySelector('.copy-table')", "名称 Name\t说明\t公式\r\n中文 🧪\t\"第一行\n第二行 \"\"引号\"\" &lt; &#65;\"\t$v=s/t$\r\n空值\t\t=SUM(1,2)", ".copy-table", ".table-block-status");
        await EvalAsync<bool>("(() => { document.querySelector('.table-copy-options').click(); return true; })()");
        await CopyReadingAndCheckAsync("document.querySelector('.copy-table-markdown')", "| 名称 Name | 说明 | 公式 |\n| --- | --- | --- |\n| 中文 🧪 | 第一行<br>第二行 \"引号\" &amp;lt; &amp;#65; | $v=s/t$ |\n| 空值 |  | =SUM(1,2) |", ".copy-table", ".table-block-status");
        Check(await EvalAsync<bool>("[...document.querySelectorAll('.table-block')].at(-1).querySelector('.copy-table').disabled && [...document.querySelectorAll('.table-block')].at(-1).querySelector('.copy-table-markdown').disabled"),
            "a table without any cells disables both copy entry points");
        int requests = await EvalAsync<int>("__readingRequests.length");
        await EvalAsync<bool>("(() => { [...document.querySelectorAll('.copy-table')].at(-1).click(); return true; })()");
        Check(await EvalAsync<int>("__readingRequests.length") == requests, "clicking an empty table cannot replace the native clipboard with an empty payload");
    }

    private async Task CheckTableFailureAndNavigationAsync(Guid chat, ConversationMessageViewModel answer)
    {
        await WaitAsync("!document.querySelector('.copy-message').dataset.copyState", "earlier whole-message feedback expires before table-failure isolation is inspected");
        await EvalAsync<bool>("(() => { window.__readingCapturePost=chrome.webview.postMessage; chrome.webview.postMessage=request=>{ if(request.type==='copy') window.__readingHeld=request; else __readingCapturePost(request); }; document.querySelector('.copy-table').click(); return true; })()");
        var held = await EvalAsync<JsonElement>("__readingHeld");
        Check(await EvalAsync<bool>("!document.querySelector('.copy-table').dataset.copyState"), "sending a request without native acknowledgement does not show table success");
        PostReadingReceipt(chat, held, success: false);
        await WaitAsync("document.querySelector('.copy-table').dataset.copyState==='error'", "simulated failed native receipt reaches table-local feedback");
        Check(await EvalAsync<bool>("document.querySelector('.table-block-status').textContent==='复制失败，请重试。' && !document.querySelector('.copy-table').hasAttribute('title') && !document.querySelector('.copy-message').dataset.copyState"),
            "table failure remains local, accessible and free of duplicate global/title feedback");
        await EvalAsync<bool>("(() => { chrome.webview.postMessage=__readingCapturePost; return true; })()");
        string source = held.GetProperty("text").GetString()!;
        await CopyReadingAndCheckAsync("document.querySelector('.copy-table')", source, ".copy-table", ".table-block-status");
        await EvalAsync<bool>("(() => { chrome.webview.postMessage=request=>{ if(request.type==='copy') window.__readingHeld=request; else __readingCapturePost(request); }; document.querySelector('.copy-table').click(); return true; })()");
        held = await EvalAsync<JsonElement>("__readingHeld");
        await EvalAsync<bool>("(() => { window.__readingTableArticle=document.querySelector('article'); window.__readingTableBlock=document.querySelector('.table-block'); return true; })()");
        var otherChat = Guid.NewGuid();
        _transcript.ShowConversation(otherChat, [Message(otherChat, "assistant", "OTHER_TABLE_CHAT\n\n| 新表 | 值 |\n| --- | --- |\n| 新值 | 2 |")]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('OTHER_TABLE_CHAT') && document.querySelectorAll('.table-block').length===1", "navigation opens an independent table in another conversation");
        PostReadingReceipt(chat, held, success: true);
        await Task.Delay(100);
        Check(await EvalAsync<bool>("!document.querySelector('.copy-table').dataset.copyState"), "late old-conversation table acknowledgement cannot update a new table");
        await EvalAsync<bool>("(() => { chrome.webview.postMessage=__readingCapturePost; return true; })()");
        _transcript.ShowConversation(chat, [answer]);
        await WaitAsync("document.querySelectorAll('.table-block').length>=2 && document.querySelector('article')===__readingTableArticle", "cached original table DOM is restored");
        Check(await EvalAsync<bool>("document.querySelector('.table-block')===__readingTableBlock && document.querySelectorAll('.table-block-header').length===document.querySelectorAll('.table-block').length && !document.querySelector('.copy-table').dataset.copyState"),
            "cached tables reuse their controls exactly once and clear old receipt feedback");
    }

    private void PostReadingReceipt(Guid chat, JsonElement request, bool success) =>
        _transcript.Browser.CoreWebView2.PostWebMessageAsJson(JsonSerializer.Serialize(new
        {
            type = "copyResult", conversationId = chat, requestId = request.GetProperty("requestId").GetString(), success
        }));

    private async Task CheckTableGeometryAndLanguageAsync()
    {
        await EvalAsync<bool>("(() => { const n=__transcriptSmoke.textNode(document.querySelector('article'),'TABLE_BEFORE'); getSelection().setBaseAndExtent(n,0,n,12); window.__readingSelection=transcriptSelectionText(); return true; })()");
        UiText.Initialize("en");
        await WaitAsync("document.documentElement.lang==='en' && /Copy/.test(document.querySelector('.copy-table').getAttribute('aria-label'))", "table copy controls translate live to English");
        Check(await EvalAsync<bool>("transcriptSelectionText()===__readingSelection && document.querySelector('.table-block')===__readingTableBlock"),
            "table localization preserves source DOM and the user's selected prose");
        _transcript.ClearSelection();
        UiText.Initialize("zh-CN");
        await WaitAsync("document.documentElement.lang==='zh-CN'", "Chinese labels return before table geometry capture");
        var geometry = new List<JsonElement>();
        foreach (int width in new[] { 980, 480 })
        {
            _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(width, 850));
            await Task.Delay(180);
            await EvalAsync<bool>("(() => { scrollTo(0,0); document.querySelector('.table-copy-options').click(); return true; })()");
            var value = await EvalAsync<JsonElement>("""
                (() => ({viewport:innerWidth,page:document.documentElement.scrollWidth,blocks:[...document.querySelectorAll('.table-block')].map(block=>{
                  const b=block.getBoundingClientRect(), h=block.querySelector('header').getBoundingClientRect(), s=block.querySelector('.table-scroll').getBoundingClientRect();
                  return {width:b.width,toolbarAbove:h.bottom<=s.top+1,buttons:[...block.querySelectorAll('.table-block-actions > button')].map(button=>{const r=button.getBoundingClientRect();return r.left>=b.left-1 && r.right<=b.right+1 && r.top>=h.top && r.bottom<=h.bottom+1;})};
                }),menu:(()=>{const r=document.querySelector('.table-copy-menu').getBoundingClientRect();return {left:r.left,right:r.right,top:r.top,bottom:r.bottom};})()}))()
                """);
            geometry.Add(value);
            Check(value.GetProperty("page").GetDouble() <= value.GetProperty("viewport").GetDouble() + 1, "table copy controls do not widen the page at width " + width);
            Check(value.GetProperty("blocks").EnumerateArray().All(block => block.GetProperty("toolbarAbove").GetBoolean()
                && block.GetProperty("buttons").EnumerateArray().All(button => button.GetBoolean())), "table controls fit above their cell surface at width " + width);
            Check(value.GetProperty("menu").GetProperty("left").GetDouble() >= 0 && value.GetProperty("menu").GetProperty("right").GetDouble() <= value.GetProperty("viewport").GetDouble() + 1,
                "table format menu stays inside the native viewport at width " + width);
            await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), $"kynxa-transcript-content-table-{width}.png"));
            await BlockCopyKeyAsync("Escape", "Escape", 27);
            Check(await EvalAsync<bool>("document.querySelector('.table-copy-options').getAttribute('aria-expanded')==='false'"), "trusted Escape closes the table format menu");
        }
        _metrics["contentTableGeometry"] = geometry;
        await EvalAsync<bool>("(() => { document.querySelector('.table-copy-options').focus(); return true; })()");
        await BlockCopyKeyAsync("ArrowDown", "ArrowDown", 40);
        Check(await EvalAsync<bool>("document.querySelector('.table-copy-options').getAttribute('aria-expanded')==='true' && document.activeElement===document.querySelector('.copy-table-markdown')"),
            "trusted ArrowDown opens the table format menu and focuses its Markdown action");
        await BlockCopyKeyAsync("Escape", "Escape", 27);
        Check(await EvalAsync<bool>("document.activeElement===document.querySelector('.table-copy-options')"),
            "closing the format menu returns keyboard focus to its options button");
        var wideChat = Guid.NewGuid();
        string wideTable = "WIDE_TABLE\n\n| " + string.Join(" | ", Enumerable.Range(1, 16).Select(index => $"列{index:00}")) + " |\n| "
            + string.Join(" | ", Enumerable.Repeat("---", 16)) + " |\n"
            + string.Join("\n", Enumerable.Range(1, 5).Select(row => "| "
                + string.Join(" | ", Enumerable.Range(1, 15).Select(column => $"R{row:00}C{column:00}_中文English测试数据"))
                + $" | 终点_END_{row:00} |"))
            + "\n\nSMALL_TABLE\n\n| 名称 | 说明 |\n| --- | --- |\n| 中文 | 简短说明 |\n| 长说明 | "
            + string.Concat(Enumerable.Repeat("中文说明 English paragraph 可以自然换行。", 12)) + " |\n| 连续字符 | "
            + new string('X', 300) + " |\n\nCOMPACT_THREE_COLUMNS\n\n| 类型 | 表达式 | 描述 |\n| --- | --- | --- |\n| 速度 | $v=s/t$ | "
            + string.Concat(Enumerable.Repeat("LONG_TOKEN_0123456789_", 80)) + " |\n| 平方 | $x^2$ | 三列小表保持随聊天区自动换行 |";
        _transcript.ShowConversation(wideChat, [Message(wideChat, "assistant", wideTable)]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('WIDE_TABLE') && document.querySelector('.table-block table').querySelectorAll('th').length===16 && document.querySelectorAll('.table-block').length===3 && document.querySelectorAll('.katex').length===2", "wide and compact Chinese/English tables render through the production Markdown path");
        var wideGeometry = new List<JsonElement>();
        foreach (int width in new[] { 480, 980, 1600, 2400 })
        {
            _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(width, 850));
            await Task.Delay(180);
            await EvalAsync<bool>("(() => { scrollTo(0,0); document.querySelector('.table-scroll').scrollLeft=0; return true; })()");
            var value = await EvalAsync<JsonElement>("""
                (() => {
                  const blocks=[...document.querySelectorAll('.table-block')],table=blocks[0].querySelector('table'),scroll=blocks[0].querySelector('.table-scroll');
                  const cells=[...table.querySelectorAll('tbody td:not(:last-child)')];
                  const wordLines=cells.map(cell=>{const text=cell.firstChild,offset=text.textContent.indexOf('English'),range=document.createRange();range.setStart(text,offset);range.setEnd(text,offset+7);return range.getClientRects().length;});
                  const last=table.querySelector('tbody td:last-child'),endRange=document.createRange();endRange.selectNodeContents(last);
                  const small=blocks[1].querySelector('.table-scroll'),longCell=small.querySelector('tbody tr:nth-child(2) td:last-child'),tokenCell=small.querySelector('tbody tr:nth-child(3) td:last-child');
                  const compact=blocks[2].querySelector('.table-scroll'),compactWord=compact.querySelector('tbody tr td:last-child');
                  return {viewport:innerWidth,page:document.documentElement.scrollWidth,client:scroll.clientWidth,scroll:scroll.scrollWidth,
                    cellWidths:cells.map(cell=>cell.getBoundingClientRect().width),wordLines,endLines:endRange.getClientRects().length,
                    smallClient:small.clientWidth,smallScroll:small.scrollWidth,longHeight:longCell.getBoundingClientRect().height,
                    singleLineHeight:parseFloat(getComputedStyle(longCell).lineHeight),tokenScroll:tokenCell.scrollWidth,tokenClient:tokenCell.clientWidth,
                    compactClient:compact.clientWidth,compactScroll:compact.scrollWidth,compactWordHeight:compactWord.getBoundingClientRect().height,compactLineHeight:parseFloat(getComputedStyle(compactWord).lineHeight),
                    toolbarRight:blocks[0].querySelector('.table-block-actions').getBoundingClientRect().right,frameRight:blocks[0].getBoundingClientRect().right};
                })()
                """);
            wideGeometry.Add(value);
            Check(value.GetProperty("scroll").GetDouble() > value.GetProperty("client").GetDouble() + 100
                && value.GetProperty("page").GetDouble() <= value.GetProperty("viewport").GetDouble() + 1,
                "sixteen readable columns scroll locally without widening the page at width " + width);
            Check(value.GetProperty("cellWidths").EnumerateArray().All(cell => cell.GetDouble() >= 150)
                && value.GetProperty("wordLines").EnumerateArray().All(lines => lines.GetInt32() == 1)
                && value.GetProperty("endLines").GetInt32() == 1,
                "wide-table identifiers, English words and END markers are not squeezed into fragments at width " + width);
            Check(value.GetProperty("smallScroll").GetDouble() <= value.GetProperty("smallClient").GetDouble() + 1
                && value.GetProperty("longHeight").GetDouble() > value.GetProperty("singleLineHeight").GetDouble() * 2
                && value.GetProperty("tokenScroll").GetDouble() <= value.GetProperty("tokenClient").GetDouble() + 1,
                "compact tables stay fitted while long paragraphs and unbroken values can wrap at width " + width);
            Check(value.GetProperty("compactScroll").GetDouble() <= value.GetProperty("compactClient").GetDouble() + 1
                && value.GetProperty("compactWordHeight").GetDouble() > value.GetProperty("compactLineHeight").GetDouble() * 2,
                "three-column tables with short formulas still fit and wrap long words at width " + width);
            Check(value.GetProperty("toolbarRight").GetDouble() <= value.GetProperty("frameRight").GetDouble() + 1,
                "wide table copy controls stay inside their fixed toolbar at width " + width);
            Check(await EvalAsync<bool>("(() => { const scroll=document.querySelector('.table-scroll'); scroll.scrollLeft=scroll.scrollWidth; const last=document.querySelector('tbody td:last-child').getBoundingClientRect(),frame=scroll.getBoundingClientRect();return last.right<=frame.right+1 && last.right>frame.left; })()"),
                "horizontal scrolling reaches the final column at width " + width);
            await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), $"kynxa-transcript-content-table-wide-{width}.png"));
        }
        _metrics["contentWideTableGeometry"] = wideGeometry;
        _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 850));
    }

    private async Task CheckLongContentReadingAsync()
    {
        var chat = Guid.NewGuid();
        string longLine = "LONG_SINGLE_LINE 中文English_" + string.Concat(Enumerable.Repeat("abcdefghijklmnopqrstuvwxyz0123456789_", 65));
        string source = string.Join("\n", Enumerable.Range(1, 64).Select(index => $"{index:000} 原样 source 行\tvalue_{index}")) + "\n" + longLine;
        string markdown = "LONG_BEFORE 需要保留的正文\n\n```text\n" + source + "\n```\n\nLONG_AFTER 阅读位置\n\n```text\nSHORT_BLOCK 短内容\n```\n\n"
            + string.Join("\n\n", Enumerable.Range(1, 35).Select(index => $"TAIL_{index:000} 滚动锚点后文"));
        var answer = Message(chat, "assistant", markdown);
        string original = JsonSerializer.Serialize(answer.Message);
        _transcript.ShowConversation(chat, [answer]);
        await WaitAsync("document.querySelectorAll('.content-block').length===2 && !!document.querySelector('[data-block-action=wrap]')", "long and short source blocks expose reading controls");
        await EvalAsync<bool>("(() => { window.__readingLong=document.querySelector('.content-block'); window.__readingLongCode=__readingLong.querySelector('code'); window.__readingLongBody=document.querySelector('.message-body'); __transcriptSmoke.scroller().dispatchEvent(new WheelEvent('wheel',{deltaY:-120,bubbles:true})); scrollTo(0,0); return true; })()");
        Check(await EvalAsync<bool>("__readingLong.dataset.collapsed==='false' && __readingLong.querySelector('[data-block-action=fold]').getAttribute('aria-expanded')==='true' && !__readingLong.querySelector('[data-block-action=fold]').hidden"),
            "completed long content starts fully expanded and offers explicit folding");
        Check(await EvalAsync<bool>("document.querySelectorAll('.content-block')[1].querySelector('[data-block-action=fold]').hidden"),
            "short content has no unnecessary collapse control");
        Check(await EvalAsync<string>("__readingLongCode.textContent") == source,
            "reading controls do not rewrite original newlines, tabs, Unicode or long source lines");
        Check(await EvalAsync<bool>("__readingLong.querySelector('.content-block-lines').dataset.lineCount==='65'"), "line count reports source lines rather than wrapped visual lines");
        Check(await EvalAsync<bool>("__readingLong.dataset.wrap==='false' && __readingLong.querySelector('pre').scrollWidth>__readingLong.querySelector('pre').clientWidth+100"),
            "default source mode preserves a very long line and scrolls inside its frame");
        await EvalAsync<bool>("(() => { __readingLong.querySelector('[data-block-action=wrap]').click(); return true; })()");
        await WaitAsync("__readingLong.dataset.wrap==='true' && __readingLong.querySelector('[data-block-action=wrap]').getAttribute('aria-pressed')==='true'", "wrap is an explicit accessible toggle");
        Check(await EvalAsync<bool>("__readingLong.querySelector('pre').scrollWidth<=__readingLong.querySelector('pre').clientWidth+1 && document.documentElement.scrollWidth<=innerWidth+1"),
            "wrapped long lines remain within both code frame and page");
        Check(await EvalAsync<string>("__readingLongCode.textContent") == source, "wrapping leaves copied source line boundaries unchanged");
        await EvalAsync<bool>("(() => { __readingLong.querySelector('[data-block-action=wrap]').click(); return true; })()");
        await WaitAsync("__readingLong.dataset.wrap==='false'", "source mode can be restored after wrapping");
        double expandedHeight = await EvalAsync<double>("__readingLong.querySelector('pre').getBoundingClientRect().height");
        double topBefore = await EvalAsync<double>("__readingLong.getBoundingClientRect().top");
        await EvalAsync<bool>("(() => { __readingLong.querySelector('[data-block-action=fold]').click(); return true; })()");
        await WaitAsync("__readingLong.dataset.collapsed==='true' && __readingLong.querySelector('[data-block-action=fold]').getAttribute('aria-expanded')==='false'", "long content folds only after the user requests it");
        double foldedHeight = await EvalAsync<double>("__readingLong.querySelector('pre').getBoundingClientRect().height");
        Check(foldedHeight < expandedHeight / 2 && foldedHeight > 100, "folded preview is shorter while retaining several readable source lines");
        var foldedGeometry = await EvalAsync<JsonElement>("""
            (() => {
              const node=__transcriptSmoke.textNode(__readingLongCode,'012 原样'), element=__readingLong.querySelector('pre'), pre=element.getBoundingClientRect();
              const bounds=token=>{const offset=node.textContent.indexOf(token),range=document.createRange();range.setStart(node,offset);range.setEnd(node,offset+3);return range.getBoundingClientRect();};
              return {lastLineBottom:bounds('012 原样').bottom,nextLineTop:bounds('013 原样').top,clipBottom:pre.top+element.clientHeight,
                outerBottom:pre.bottom,clientHeight:element.clientHeight,offsetHeight:element.offsetHeight,lineHeight:getComputedStyle(element).lineHeight};
            })()
            """);
        _metrics["contentFoldGeometry"] = foldedGeometry;
        await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), "kynxa-transcript-content-fold-source.png"));
        // The horizontal scrollbar occupies outer height but is outside the clipped source viewport.
        // 横向滚动条占据外框高度，但不属于源码的可见裁切区域。
        Check(foldedGeometry.GetProperty("lastLineBottom").GetDouble() <= foldedGeometry.GetProperty("clipBottom").GetDouble() + 1
            && foldedGeometry.GetProperty("nextLineTop").GetDouble() >= foldedGeometry.GetProperty("clipBottom").GetDouble() - 1,
            "the folded source shows twelve complete lines without a partly clipped thirteenth line");
        await Task.Delay(80);
        Check(Math.Abs(await EvalAsync<double>("__readingLong.getBoundingClientRect().top") - topBefore) < 2,
            "folding a visible block preserves its reading/toolbar anchor instead of jumping to the bottom");
        await CopyReadingAndCheckAsync("__readingLong.querySelector('.copy-block')", source, ".copy-block", ".content-block-status");
        Check(await EvalAsync<bool>("(() => { const c=__readingLong.querySelector('.copy-block').getBoundingClientRect(),s=__readingLong.querySelector('.content-block-status').getBoundingClientRect();return Math.abs((c.top+c.bottom)/2-(s.top+s.bottom)/2)<2; })()"),
            "brief successful copy feedback stays on the same row beside the content-copy button");
        Check(await EvalAsync<bool>("__readingLong.dataset.collapsed==='true' && __readingLongCode===__readingLong.querySelector('code')"),
            "copying a folded block obtains its entire source without expanding or replacing its DOM");
        await EvalAsync<bool>("(() => { const node=__transcriptSmoke.textNode(__readingLongCode,'001 原样'); getSelection().setBaseAndExtent(node,0,node,6); window.__readingCodeSelection=transcriptSelectionText(); __readingLong.querySelector('[data-block-action=wrap]').click(); return true; })()");
        Check(await EvalAsync<bool>("__readingLong.dataset.wrap==='false' && transcriptSelectionText()===__readingCodeSelection && __readingLongCode===__readingLong.querySelector('code')"),
            "an active source selection prevents a reflowing reading toggle and keeps the exact range intact");
        await WaitAsync("!__readingLong.querySelector('.copy-block').dataset.copyState", "copy feedback expires before the selection instruction is inspected");
        await EvalAsync<bool>("(() => { __readingLong.querySelector('[data-block-action=wrap]').click(); return true; })()");
        Check(await EvalAsync<bool>("__readingLong.querySelector('.content-block-status').dataset.readingState==='blocked' && !!__readingLong.querySelector('.content-block-status').textContent"),
            "a blocked reflow explains locally that the selected text must be cleared first");
        _transcript.ClearSelection();
        await EvalAsync<bool>("(() => { __readingLong.querySelector('[data-block-action=fold]').click(); __readingLong.querySelector('[data-block-action=wrap]').click(); return true; })()");
        await WaitAsync("__readingLong.dataset.collapsed==='false' && __readingLong.dataset.wrap==='true'", "clearing selection allows expansion and wrapping again");
        await EvalAsync<bool>("(() => { __readingLong.querySelector('[data-block-action=wrap]').focus(); return true; })()");
        await BlockCopyKeyAsync(" ", "Space", 32);
        await WaitAsync("__readingLong.dataset.wrap==='false'", "trusted keyboard Space toggles source wrapping");
        await BlockCopyKeyAsync("Enter", "Enter", 13);
        await WaitAsync("__readingLong.dataset.wrap==='true'", "trusted keyboard Enter restores wrapping on the same control");
        await CheckLongBlockCacheAndLanguageAsync(chat, answer);
        await CheckLongBlockGeometryAsync();
        Check(JsonSerializer.Serialize(answer.Message) == original,
            "wrap/fold, copy, selection, localization and cache leave the formal message unchanged");
        await CheckLongBlockStreamingAsync(source);
    }

    private async Task CheckLongBlockCacheAndLanguageAsync(Guid chat, ConversationMessageViewModel answer)
    {
        await EvalAsync<bool>("(() => { __readingLong.querySelector('[data-block-action=fold]').click(); const node=__transcriptSmoke.textNode(__readingLongBody,'LONG_BEFORE'); getSelection().setBaseAndExtent(node,0,node,11); window.__readingLanguageSelection=transcriptSelectionText(); return true; })()");
        UiText.Initialize("en");
        await WaitAsync("document.documentElement.lang==='en' && /line/i.test(__readingLong.querySelector('.content-block-lines').textContent)", "line count and reading controls translate live to English");
        Check(await EvalAsync<bool>("transcriptSelectionText()===__readingLanguageSelection && __readingLong===document.querySelector('.content-block') && __readingLong.dataset.collapsed==='true' && __readingLong.dataset.wrap==='true'"),
            "language changes preserve selected prose, content DOM and explicit reading preferences");
        _transcript.ClearSelection();
        var otherChat = Guid.NewGuid();
        _transcript.ShowConversation(otherChat, [Message(otherChat, "assistant", "READING_OTHER_CHAT\n\n```text\nOTHER SOURCE\n```")]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('READING_OTHER_CHAT')", "a new conversation replaces the long content fixture");
        UiText.Initialize("zh-CN");
        _transcript.ShowConversation(chat, [answer]);
        await WaitAsync("document.querySelector('.content-block')===__readingLong && document.documentElement.lang==='zh-CN'", "cached long block returns with the current language");
        Check(await EvalAsync<bool>("__readingLong.dataset.wrap==='true' && __readingLong.dataset.collapsed==='true' && __readingLong.querySelectorAll('[data-block-action=wrap]').length===1 && __readingLong.querySelectorAll('[data-block-action=fold]').length===1"),
            "cached long block retains wrap/fold preferences without duplicating its control group");
        Check(await EvalAsync<bool>("/行/.test(__readingLong.querySelector('.content-block-lines').textContent) && !/Expand/.test(__readingLong.querySelector('[data-block-action=fold]').textContent)"),
            "a detached cached block localizes when restored to another language");
    }

    private async Task CheckLongBlockGeometryAsync()
    {
        var geometry = new List<JsonElement>();
        foreach (int width in new[] { 980, 480 })
        {
            _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(width, 850));
            await Task.Delay(180);
            await EvalAsync<bool>("(() => { scrollTo(0,0); return true; })()");
            var value = await EvalAsync<JsonElement>("""
                (() => ({viewport:innerWidth,page:document.documentElement.scrollWidth,blocks:[...document.querySelectorAll('.content-block')].map(block=>{
                  const b=block.getBoundingClientRect(),h=block.querySelector('header').getBoundingClientRect(),p=block.querySelector('pre').getBoundingClientRect();
                  return {toolbarAbove:h.bottom<=p.top+1,buttons:[...block.querySelectorAll('header button')].filter(button=>!button.hidden).map(button=>{const r=button.getBoundingClientRect();return r.left>=b.left-1 && r.right<=b.right+1 && r.top>=h.top && r.bottom<=h.bottom+1;})};
                })}))()
                """);
            geometry.Add(value);
            Check(value.GetProperty("page").GetDouble() <= value.GetProperty("viewport").GetDouble() + 1, "long content controls do not widen the native page at width " + width);
            Check(value.GetProperty("blocks").EnumerateArray().All(block => block.GetProperty("toolbarAbove").GetBoolean()
                && block.GetProperty("buttons").EnumerateArray().All(button => button.GetBoolean())), "wrap/fold/copy controls remain above source and within their rounded frame at width " + width);
            await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), $"kynxa-transcript-content-long-{width}.png"));
        }
        _metrics["contentLongGeometry"] = geometry;
    }

    private async Task CheckLongBlockStreamingAsync(string source)
    {
        var chat = Guid.NewGuid();
        var answer = Message(chat, "assistant", "STREAM_BEFORE\n\n```text\n" + source, "streaming");
        _transcript.ShowConversation(chat, [answer]);
        await WaitAsync("document.querySelector('.content-block pre')?.dataset.codeComplete==='false'", "open streaming source uses the incomplete code surface");
        Check(await EvalAsync<bool>("document.querySelector('.content-block').dataset.collapsed==='false' && document.querySelector('[data-block-action=fold]').hidden"),
            "an incomplete streamed block remains expanded and cannot hide arriving source");
        await EvalAsync<bool>("(() => { document.querySelector('[data-block-action=wrap]').click(); window.__readingStreamArticle=document.querySelector('article'); return true; })()");
        await WaitAsync("document.querySelector('.content-block').dataset.wrap==='true'", "streaming source permits manual wrapping");
        answer.Message.Content += "\nSTREAM_VISIBLE_APPEND";
        answer.Refresh();
        await WaitAsync("document.querySelector('.content-block code').textContent.includes('STREAM_VISIBLE_APPEND')", "next streaming source append reaches the rendered block");
        Check(await EvalAsync<bool>("document.querySelector('.content-block').dataset.wrap==='true' && document.querySelector('article')===__readingStreamArticle"),
            "streaming updates retain the user's wrap preference and message identity");
        await EvalAsync<bool>("(() => { const node=__transcriptSmoke.textNode(document.querySelector('.content-block code'),'001 原样'); getSelection().setBaseAndExtent(node,0,node,6); window.__readingStreamSelection=transcriptSelectionText(); window.__readingStreamCode=document.querySelector('.content-block code'); return true; })()");
        answer.Message.Content += "\nSTREAM_DEFERRED_APPEND\n```\n\nSTREAM_FINISHED";
        answer.Message.Status = "completed";
        answer.Refresh();
        await WaitAsync("transcriptState().pending", "source selection holds final incoming content pending");
        Check(await EvalAsync<bool>("document.querySelector('.content-block code')===__readingStreamCode && transcriptSelectionText()===__readingStreamSelection && !__readingStreamCode.textContent.includes('STREAM_DEFERRED_APPEND')"),
            "completion does not replace selected code or expose a newer hidden copy snapshot");
        _transcript.ClearSelection();
        await WaitAsync("document.querySelector('.content-block pre')?.dataset.codeComplete==='true' && document.querySelector('.content-block code').textContent.includes('STREAM_DEFERRED_APPEND')", "clearing selection applies the closed final block");
        Check(await EvalAsync<bool>("document.querySelector('.content-block').dataset.wrap==='true' && !document.querySelector('[data-block-action=fold]').hidden && document.querySelector('.content-block').dataset.collapsed==='false'"),
            "a closed final block becomes manually collapsible without automatically hiding its content");
        await EvalAsync<bool>("(() => { document.querySelector('[data-block-action=fold]').click(); return true; })()");
        await WaitAsync("document.querySelector('.content-block').dataset.collapsed==='true'", "final streamed source can be folded after completion");
        await CopyReadingAndCheckAsync("document.querySelector('.copy-block')", source + "\nSTREAM_VISIBLE_APPEND\nSTREAM_DEFERRED_APPEND", ".copy-block", ".content-block-status");
    }
}
