using System.Reflection;
using System.Text.Json;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;
using Windows.ApplicationModel.DataTransfer;

namespace TranscriptUiSmoke;

public partial class App
{
    // Exercise search through the production host and real WebView, without altering formal source messages.
    // 通过正式宿主与真实 WebView 验证搜索，不改写正式消息源码。
    private async Task CheckBodySearchAsync()
    {
        string language = UiText.Language;
        string palette = AppearanceService.Current.Id;
        string mode = AppearanceService.Mode;
        string? accent = AppearanceService.CustomAccent;
        double fontSize = AppearanceService.ChatFontSize;
        var size = _window!.AppWindow.Size;
        try
        {
            await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Emulation.setFocusEmulationEnabled", "{\"enabled\":true}");
            UiText.Initialize("zh-CN");
            await CheckBodySearchBeforeFirstRenderAsync();
            await CheckBodySearchContentAsync();
            await CheckBodySearchReadingAsync();
            await CheckBodySearchStreamingAsync();
            await CheckBodySearchContinuousIndexAsync();
            await CheckBodySearchBoundariesAsync();
            await CheckBodySearchLayoutAsync();
            _metrics["bodySearchScope"] = "real WinUI/WebView2 and native host commands; synthetic conversation source; native clipboard; DOM keyboard events plus native shortcut checks in Shell fixture";
        }
        finally
        {
            _transcript.CloseFindText();
            _transcript.ClearSelection();
            UiText.Initialize(language);
            AppearanceService.ApplyPreferences(palette, mode, fontSize, accent);
            _window.AppWindow.Resize(size);
            await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Emulation.setFocusEmulationEnabled", "{\"enabled\":false}");
        }
    }

    private async Task CheckBodySearchBeforeFirstRenderAsync()
    {
        var originalTranscript = _transcript;
        var renderingField = typeof(ConversationTranscript).GetField("_rendering", BindingFlags.Instance | BindingFlags.NonPublic)!;
        foreach (Guid? conversationId in new Guid?[] { Guid.NewGuid(), null })
        {
            var coldTranscript = new ConversationTranscript();
            var coldWindow = new Window { Title = "KYNXA cold body-search fixture", Content = coldTranscript };
            string expectedId = conversationId?.ToString() ?? "";
            ConversationMessageViewModel[] rows = conversationId is Guid chat
                ? [Message(chat, "assistant", "COLD_SEARCH_MATCH 首次正文")]
                : [];
            var opened = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            void FindStateChanged(object? sender, EventArgs e)
            {
                if (coldTranscript.IsFindTextOpen) opened.TrySetResult();
            }
            coldTranscript.FindTextStateChanged += FindStateChanged;
            try
            {
                // Hold only the fixture's first flush, reproducing slow startup without adding a production hook or rewriting its DOM.
                // 仅暂缓夹具的首次正文刷新，受控复现慢启动，不增加生产测试开关，也不改写正式 DOM。
                renderingField.SetValue(coldTranscript, true);
                coldTranscript.ShowConversation(conversationId, rows);
                Check(!coldTranscript.Ready.IsCompleted, "cold find request is issued before WebView readiness");
                coldTranscript.OpenFindText();
                _transcript = coldTranscript;
                coldWindow.AppWindow.Resize(new Windows.Graphics.SizeInt32(500, 350));
                coldWindow.AppWindow.Show(activateWindow: false);
                await coldTranscript.Ready.WaitAsync(TimeSpan.FromSeconds(40));
                await coldTranscript.Browser.ExecuteScriptAsync(File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "Checks.js")));
                await opened.Task.WaitAsync(TimeSpan.FromSeconds(10));
                await WaitAsync("kynxaTranscriptSearch.state().open && kynxaTranscriptSearch.state().conversationId==="
                    + JsonSerializer.Serialize(expectedId) + " && transcriptState().conversationId===" + JsonSerializer.Serialize(expectedId)
                    + " && transcriptState().messageCount===0", "cold search opens with the current conversation identity before any body render");
                Check(coldTranscript.IsFindTextOpen, "native host accepts the cold search state for the correct conversation");
                await SetBodySearchQueryAsync("COLD_SEARCH_MATCH", 0);
                await EvalAsync<bool>("(() => {window.__coldBodySearchRenders=0;chrome.webview.addEventListener('message',event=>{if(event.data?.type==='render')__coldBodySearchRenders++;});return true;})()");

                renderingField.SetValue(coldTranscript, false);
                coldTranscript.ShowConversation(conversationId, rows, openAtBottom: false);
                await WaitAsync("window.__coldBodySearchRenders>0 && kynxaTranscriptSearch.state().open && kynxaTranscriptSearch.state().query==='COLD_SEARCH_MATCH'"
                    + " && kynxaTranscriptSearch.state().conversationId===" + JsonSerializer.Serialize(expectedId)
                    + " && !kynxaTranscriptSearch.state().searching && kynxaTranscriptSearch.state().count===" + rows.Length
                    + " && transcriptState().messageCount===" + rows.Length,
                    "the first native body snapshot preserves the cold search bar, identity and keyword");
                Check(coldTranscript.IsFindTextOpen && (rows.Length == 0 || rows[0].Message.Content == "COLD_SEARCH_MATCH 首次正文"),
                    "first-render search preserves native state and authoritative message content");
                _metrics[conversationId is null ? "bodySearchColdEmpty" : "bodySearchColdConversation"] = await EvalAsync<JsonElement>(
                    "({state:kynxaTranscriptSearch.state(),transcript:transcriptState(),nativeRenders:__coldBodySearchRenders})");
            }
            finally
            {
                coldTranscript.FindTextStateChanged -= FindStateChanged;
                renderingField.SetValue(coldTranscript, false);
                coldTranscript.Dispose();
                coldWindow.Close();
                _transcript = originalTranscript;
            }
        }
        _metrics["bodySearchColdScope"] = "real fresh WinUI/WebView2; native find requested before Ready; first body flush held by a fixture-only reflection gate; GUID and empty conversation identities; no operating-system shortcut or real provider delay";
    }

    private async Task SetBodySearchQueryAsync(string query, int expectedCount)
    {
        await EvalAsync<bool>("(() => { const input=document.getElementById('transcript-search-input');input.value="
            + JsonSerializer.Serialize(query) + ";input.dispatchEvent(new Event('input',{bubbles:true}));return true; })()");
        await WaitAsync("(() => { const state=kynxaTranscriptSearch.state();return state.query===" + JsonSerializer.Serialize(query)
            + " && !state.searching && state.count===" + expectedCount + "; })()", "search settles with exact match count for " + query);
    }

    private Task ClickBodySearchAsync(string action) => EvalAsync<bool>("(() => { document.querySelector('[data-search-action="
        + action + "]').click();return true; })()");

    private Task BodySearchKeyAsync(string key, bool shift = false) => EvalAsync<bool>("(() => {document.getElementById('transcript-search-input').dispatchEvent(new KeyboardEvent('keydown',{key:"
        + JsonSerializer.Serialize(key) + ",shiftKey:" + (shift ? "true" : "false") + ",bubbles:true,cancelable:true}));return true;})()");

    private async Task CheckBodySearchContentAsync()
    {
        string fence = new((char)96, 3);
        string source = "  FIRST_CODE\n\n\n    MATCH_CODE 中文English\nLAST_CODE";
        string markdown = "SEARCH_CONTENT_FIXTURE\n\n跨**样式**中文 English_TOKEN english_token\n\n特殊 !@#$%^&*()_+-=[]{}|\n\n"
            + "FORMULA_PREFIX $E=mc^2$ FORMULA_SUFFIX\n\n"
            + fence + "txt\n" + source + "\n" + fence
            + "\n\n| 表头 | Value |\n| --- | --- |\n| TABLE_MATCH中文 | 42 |\n\n> QUOTE_MATCH 引用中文\n\n"
            + fence + "mermaid\nflowchart LR\n  A[\"DIAGRAM_MATCH 中文\"] --> B[\"完成\"]\n" + fence;
        var chat = Guid.NewGuid();
        var user = Message(chat, "user", "USER_MATCH 用户消息");
        var answer = Message(chat, "assistant", markdown);
        _transcript.ShowConversation(chat, [user, answer]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('SEARCH_CONTENT_FIXTURE') && document.querySelector('.mermaid-viewport svg')!==null", "search fixture renders code, table, quote and Mermaid");
        await EvalAsync<bool>("(() => {window.__searchOriginalBody=document.querySelector('[data-role=assistant] .message-body');window.__searchOriginalNodes=[...__searchOriginalBody.querySelectorAll('p,code,td,strong')];window.__searchOriginalText=__searchOriginalBody.textContent;return true;})()");
        _transcript.OpenFindText();
        await WaitAsync("kynxaTranscriptSearch.state().open && document.activeElement.id==='transcript-search-input'", "native open command focuses the application search input");
        foreach (var fixture in new[]
        {
            (Query: "USER_MATCH", Count: 1), (Query: "跨样式中文", Count: 1),
            (Query: "english_token", Count: 2), (Query: "MATCH_CODE", Count: 1),
            (Query: "TABLE_MATCH中文", Count: 1), (Query: "QUOTE_MATCH", Count: 1),
            (Query: "DIAGRAM_MATCH", Count: 1), (Query: "!@#$%^&*()_+-=[]{}|", Count: 1),
            (Query: "NEVER_MATCHED", Count: 0), (Query: "复制整条消息", Count: 0)
        })
        {
            await SetBodySearchQueryAsync(fixture.Query, fixture.Count);
        }
        Check(await EvalAsync<bool>("document.getElementById('transcript-search').textContent.includes('没有')"), "no-result feedback stays inside the search bar");
        await WaitAsync("!!document.querySelector('.message-body .katex-html')", "the actual inline formula has separate visual and accessibility text");
        string formulaQuery = await EvalAsync<string>("(() => {const paragraph=[...document.querySelectorAll('.message-body p')].find(node=>node.textContent.includes('FORMULA_PREFIX'));const copy=paragraph.cloneNode(true);copy.querySelectorAll('.katex-mathml,annotation').forEach(node=>node.remove());return copy.textContent;})()");
        await SetBodySearchQueryAsync(formulaQuery, 1);
        Check(await EvalAsync<bool>("!kynxaTranscriptSearch.state().searching && [...CSS.highlights.values()].some(highlight=>[...highlight].some(range=>range.startContainer.isConnected && range.endContainer.isConnected))"), "search spanning prose and visual formula text settles without repeatedly rejecting the hidden MathML descendants");
        await SetBodySearchQueryAsync("E=mc^2", 0);
        await SetBodySearchQueryAsync("跨样式中文", 1);
        Check(await EvalAsync<bool>("CSS.highlights.size>=1 && [...CSS.highlights.values()].some(highlight=>[...highlight].some(range=>range.toString()==='跨样式中文'))"), "search ranges span existing bold text nodes without inserting markup");
        Check(await EvalAsync<bool>("__searchOriginalBody.textContent===__searchOriginalText && __searchOriginalNodes.every(node=>node.isConnected) && !__searchOriginalBody.querySelector('mark')"), "all search queries leave original text and existing content nodes unchanged");

        await SetBodySearchQueryAsync("English_TOKEN", 2);
        Check(await EvalAsync<int>("kynxaTranscriptSearch.state().index") == -1, "typing a keyword does not automatically activate or scroll to a match");
        await BodySearchKeyAsync("Enter");
        await WaitAsync("kynxaTranscriptSearch.state().index===0", "Enter explicitly activates the first match");
        await BodySearchKeyAsync("Enter");
        await WaitAsync("kynxaTranscriptSearch.state().index===1", "Enter advances to the next match");
        await BodySearchKeyAsync("Enter");
        await WaitAsync("kynxaTranscriptSearch.state().index===0", "next match wraps around the result list");
        await BodySearchKeyAsync("Enter", true);
        await WaitAsync("kynxaTranscriptSearch.state().index===1", "Shift+Enter navigates backwards with wrap-around");
        await ClickBodySearchAsync("previous");
        await WaitAsync("kynxaTranscriptSearch.state().index===0", "previous button navigates independently of keyboard focus");

        // Trusted pointer/key events cover clicking back into the query rather than only dispatching DOM handlers.
        // 使用可信鼠标和按键覆盖重新点击搜索框的行为，不仅直接触发 DOM 回调。
        var searchPoint = await EvalAsync<JsonElement>("(() => {const input=document.getElementById('transcript-search-input'),rect=input.getBoundingClientRect();return{x:rect.x+rect.width/2,y:rect.y+rect.height/2};})()");
        double searchX = searchPoint.GetProperty("x").GetDouble(), searchY = searchPoint.GetProperty("y").GetDouble();
        await DispatchMouse("mouseMoved", searchX, searchY);
        await DispatchMouse("mousePressed", searchX, searchY, "left", 1);
        await DispatchMouse("mouseReleased", searchX, searchY, "left");
        await BlockCopyKeyAsync("Enter", "Enter", 13);
        await WaitAsync("kynxaTranscriptSearch.state().index===1 && document.activeElement.id==='transcript-search-input'", "trusted click back into the query retains working Enter navigation and input focus");
        await BlockCopyKeyAsync("ArrowUp", "ArrowUp", 38);
        await WaitAsync("kynxaTranscriptSearch.state().index===0", "Up in the clicked query navigates to the previous result");
        await BlockCopyKeyAsync("ArrowDown", "ArrowDown", 40);
        await WaitAsync("kynxaTranscriptSearch.state().index===1", "Down in the clicked query navigates to the next result");
        await EvalAsync<bool>("(() => {document.getElementById('transcript-search-input').blur();return true;})()");
        await BlockCopyKeyAsync("F3", "F3", 114);
        await WaitAsync("kynxaTranscriptSearch.state().index===0", "F3 navigates once after the search input loses focus");
        await EvalAsync<bool>("(() => {const input=document.getElementById('transcript-search-input');input.focus();input.dispatchEvent(new CompositionEvent('compositionstart',{bubbles:true}));input.blur();return true;})()");
        await BlockCopyKeyAsync("F3", "F3", 114);
        await WaitAsync("kynxaTranscriptSearch.state().index===1", "blur clears a cancelled composition flag so future navigation is not stranded");

        await SetBodySearchQueryAsync("MATCH_CODE", 1);
        await EvalAsync<bool>("(() => {const node=__transcriptSmoke.textNode(document.querySelector('.content-block code'),'MATCH_CODE');const start=node.nodeValue.indexOf('MATCH_CODE');getSelection().setBaseAndExtent(node,start,node,start+10);window.__searchSelectionBefore=transcriptSelectionText();window.__searchSelectionTop=scrollY;return true;})()");
        await ClickBodySearchAsync("next");
        await Task.Delay(150);
        Check(await EvalAsync<bool>("transcriptSelectionText()===__searchSelectionBefore && Math.abs(scrollY-__searchSelectionTop)<2 && kynxaTranscriptSearch.state().index===-1"), "active text selection blocks result navigation without losing selected text or moving the viewport");
        Check(await EvalAsync<string>("__transcriptSmoke.copyEvent()") == "MATCH_CODE", "selection copy excludes search decorations and controls");
        _transcript.ClearSelection();
        await EvalAsync<bool>("(() => {document.querySelector('.copy-block').click();return true;})()");
        await WaitAsync("document.querySelector('.copy-block').dataset.copyState==='success'", "block copy receives the native clipboard acknowledgement while searching");
        Check(await Clipboard.GetContent().GetTextAsync() == source, "search highlights never enter copied source and consecutive blank lines remain exact");
        Check(answer.Message.Content == markdown && user.Message.Content == "USER_MATCH 用户消息", "search does not change either authoritative message source");
        await EvalAsync<bool>("(() => {document.querySelector('.table-copy-options').click();return true;})()");
        await WaitAsync("!document.querySelector('.table-copy-menu').hidden && kynxaTranscriptSearch.state().open", "table format menu opens above an existing search bar");
        await EvalAsync<bool>("(() => {document.querySelector('.copy-table-markdown').dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',isComposing:true,bubbles:true,cancelable:true}));return true;})()");
        Check(await EvalAsync<bool>("!document.querySelector('.table-copy-menu').hidden && kynxaTranscriptSearch.state().open"), "composing Escape preserves both the table menu and the underlying search");
        await BlockCopyKeyAsync("Escape", "Escape", 27);
        Check(await EvalAsync<bool>("document.querySelector('.table-copy-menu').hidden && kynxaTranscriptSearch.state().open && document.activeElement===document.querySelector('.table-copy-options')"), "first trusted Escape closes only the foreground table menu and restores its toggle focus");
        await BlockCopyKeyAsync("Escape", "Escape", 27);
        await WaitAsync("!kynxaTranscriptSearch.state().open && document.getElementById('transcript-search').hidden", "Escape closes the search bar");
        Check(await EvalAsync<int>("CSS.highlights.size") == 0, "closing search removes all search highlight ranges");
    }

    private async Task CheckBodySearchReadingAsync()
    {
        string fence = new((char)96, 3);
        var chatA = Guid.NewGuid();
        var chatB = Guid.NewGuid();
        string source = string.Join("\n", Enumerable.Range(1, 50).Select(index => $"LINE_{index:000} " + (index == 45 ? "HIDDEN_CODE_MATCH" : "reading")));
        string markdown = "SEARCH_READING_A\n\n" + string.Join("\n\n", Enumerable.Range(1, 35).Select(index => $"TOP_{index:000} 文本"))
            + "\n\n" + fence + "txt\n" + source + "\n" + fence
            + "\n\n" + fence + "mermaid\nflowchart LR\n A[\"HIDDEN_DIAGRAM_MATCH\"] --> B[\"Done\"]\n" + fence
            + "\n\n" + string.Join("\n\n", Enumerable.Range(1, 65).Select(index => $"BOTTOM_{index:000} 文本"));
        var answerA = Message(chatA, "assistant", markdown);
        var answerB = Message(chatB, "assistant", "SEARCH_READING_B 另一个会话");
        _transcript.ShowConversation(chatA, [answerA]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('SEARCH_READING_A') && !!document.querySelector('.content-block') && !!document.querySelector('.mermaid-viewport svg')", "long conversation reaches its production reading and diagram controls");
        await EvalAsync<bool>("(() => {const block=document.querySelector('.content-block');block.querySelector('[data-block-action=fold]').click();__transcriptSmoke.scroller().dispatchEvent(new WheelEvent('wheel',{deltaY:-120,bubbles:true}));document.querySelector('.message-body > p').scrollIntoView();scrollTo(0,300);window.__searchOpeningTop=scrollY;return true;})()");
        await WaitAsync("document.querySelector('.content-block').dataset.collapsed==='true' && !transcriptState().following", "reader folds a long block and pauses above latest content");
        _transcript.OpenFindText();
        await WaitAsync("kynxaTranscriptSearch.state().open", "search opens above the current reading position");
        await SetBodySearchQueryAsync("HIDDEN_CODE_MATCH", 1);
        Check(await EvalAsync<bool>("Math.abs(scrollY-__searchOpeningTop)<2 && document.querySelector('.content-block').dataset.collapsed==='true'"), "opening and typing search neither scrolls nor unfolds hidden matches");
        await ClickBodySearchAsync("next");
        await WaitAsync("document.querySelector('.content-block').dataset.collapsed==='false' && kynxaTranscriptSearch.state().index===0", "explicit navigation reveals a match beyond the twelve-line folded preview");
        Check(await EvalAsync<bool>("(() => {const node=__transcriptSmoke.textNode(document.querySelector('.content-block code'),'HIDDEN_CODE_MATCH');const range=document.createRange();const offset=node.nodeValue.indexOf('HIDDEN_CODE_MATCH');range.setStart(node,offset);range.setEnd(node,offset+17);const bounds=range.getBoundingClientRect();return bounds.top>=document.getElementById('transcript-search').getBoundingClientRect().bottom && bounds.bottom<=innerHeight;})()"), "revealed code match is visible below the search bar");
        await SetBodySearchQueryAsync("HIDDEN_DIAGRAM_MATCH", 1);
        Check(await EvalAsync<bool>("!document.querySelector('.mermaid-source-details').open"), "indexing Mermaid source does not open its details automatically");
        await ClickBodySearchAsync("next");
        await WaitAsync("document.querySelector('.mermaid-source-details').open && kynxaTranscriptSearch.state().index===0", "diagram source opens only when the reader navigates to its match");
        await EvalAsync<bool>("(() => {__transcriptSmoke.scroller().dispatchEvent(new WheelEvent('wheel',{deltaY:-100,bubbles:true}));scrollBy(0,-100);window.__searchSavedTop=scrollY;return true;})()");
        _transcript.ShowConversation(chatB, [answerB]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('SEARCH_READING_B')", "conversation B replaces A while A has an active query");
        Check(await EvalAsync<bool>("kynxaTranscriptSearch.state().query!== 'HIDDEN_DIAGRAM_MATCH' && kynxaTranscriptSearch.state().count===0"), "A query and match ranges do not leak into conversation B");
        _transcript.ShowConversation(chatA, [answerA]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('SEARCH_READING_A') && kynxaTranscriptSearch.state().query==='HIDDEN_DIAGRAM_MATCH' && !kynxaTranscriptSearch.state().searching", "returning to cached conversation A restores its own search query");
        await Task.Delay(220);
        Check(await EvalAsync<bool>("kynxaTranscriptSearch.state().open && kynxaTranscriptSearch.state().index===0 && Math.abs(scrollY-__searchSavedTop)<2 && !transcriptState().following"), "A to B to A restores both search state and exact prior reading position without jumping to the result or bottom");
        _metrics["bodySearchCachedReturn"] = await EvalAsync<JsonElement>("({state:kynxaTranscriptSearch.state(),top:scrollY,expectedTop:__searchSavedTop,following:transcriptState().following})");
        _transcript.CloseFindText();
        await WaitAsync("!kynxaTranscriptSearch.state().open", "host closes the active search bar");
    }

    private async Task CheckBodySearchStreamingAsync()
    {
        var chat = Guid.NewGuid();
        string content = "SEARCH_STREAM\n\n" + string.Join("\n\n", Enumerable.Range(1, 80).Select(index => $"PARAGRAPH_{index:000} 文本"));
        var answer = Message(chat, "assistant", content, "streaming");
        _transcript.ShowConversation(chat, [answer]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('SEARCH_STREAM')", "streaming search fixture is visible");
        await EvalAsync<bool>("(() => {__transcriptSmoke.scroller().dispatchEvent(new WheelEvent('wheel',{deltaY:-120,bubbles:true}));scrollTo(0,300);window.__searchStreamTop=scrollY;return true;})()");
        _transcript.OpenFindText();
        await WaitAsync("kynxaTranscriptSearch.state().open", "search opens during an active streaming response");
        await SetBodySearchQueryAsync("NEW_MATCH", 0);
        for (int index = 1; index <= 20; index++)
        {
            answer.Message.Content += $"\n\nNEW_MATCH_{index:000}";
            answer.Refresh();
            await Task.Delay(20);
        }
        await WaitAsync("kynxaTranscriptSearch.state().count===20 && !kynxaTranscriptSearch.state().searching && __transcriptSmoke.bodyText().includes('NEW_MATCH_020')", "throttled search catches every match after twenty incremental native refreshes");
        Check(await EvalAsync<bool>("Math.abs(scrollY-__searchStreamTop)<2 && !transcriptState().following && kynxaTranscriptSearch.state().index===-1"), "streaming result count updates do not navigate or resume bottom following");
        await EvalAsync<bool>("(() => {const node=__transcriptSmoke.textNode(document.querySelector('.message-body'),'PARAGRAPH_001');getSelection().setBaseAndExtent(node,0,node,13);window.__searchFrozenSelection=transcriptSelectionText();return true;})()");
        answer.Message.Content += "\n\nNEW_MATCH_021";
        answer.Message.Status = "completed";
        answer.Refresh();
        await WaitAsync("transcriptState().pending", "selected streaming content defers the final DOM snapshot");
        Check(await EvalAsync<bool>("kynxaTranscriptSearch.state().count===20 && transcriptSelectionText()===__searchFrozenSelection"), "search indexes the preserved visible snapshot while the reader selects text");
        _transcript.ClearSelection();
        await WaitAsync("kynxaTranscriptSearch.state().count===21 && !transcriptState().pending && !kynxaTranscriptSearch.state().searching", "clearing the selection applies completion and updates search matches");
        await SetBodySearchQueryAsync("PARAGRAPH_", 80);
        await EvalAsync<bool>("(() => {document.getElementById('transcript-search-input').value='NEW_MATCH';document.getElementById('transcript-search-input').dispatchEvent(new Event('input',{bubbles:true}));document.getElementById('transcript-search-input').value='NEVER_STALE';document.getElementById('transcript-search-input').dispatchEvent(new Event('input',{bubbles:true}));return true;})()");
        await WaitAsync("kynxaTranscriptSearch.state().query==='NEVER_STALE' && !kynxaTranscriptSearch.state().searching && kynxaTranscriptSearch.state().count===0", "rapid query edits discard older scheduled search work");
        _transcript.CloseFindText();

        var manyChat = Guid.NewGuid();
        var many = Message(manyChat, "assistant", "SEARCH_MANY\n\n" + string.Join(" ", Enumerable.Repeat("MATCH_MANY", 2500)));
        _transcript.ShowConversation(manyChat, [many]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('SEARCH_MANY')", "large match fixture is visible");
        _transcript.OpenFindText();
        await WaitAsync("kynxaTranscriptSearch.state().open", "search opens in the many-match fixture");
        var elapsed = System.Diagnostics.Stopwatch.StartNew();
        await SetBodySearchQueryAsync("MATCH_MANY", 2500);
        _metrics["bodySearch2500MatchesMs"] = elapsed.ElapsedMilliseconds;
        Check(await EvalAsync<bool>("kynxaTranscriptSearch.state().count===2500 && !kynxaTranscriptSearch.state().searching"), "large result lists report the complete count without a silent match limit");
        _transcript.CloseFindText();
    }

    private async Task CheckBodySearchContinuousIndexAsync()
    {
        // Isolate controller scheduling from Markdown parsing with a large synthetic DOM inside the real WebView.
        // 在真实 WebView 中使用大规模合成 DOM，将搜索调度压力与 Markdown 解析开销分开验证。
        var chat = Guid.NewGuid();
        _transcript.ShowConversation(chat, [Message(chat, "assistant", "SEARCH_INDEX_STRESS")]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('SEARCH_INDEX_STRESS')", "search scheduling stress fixture is visible");
        _transcript.OpenFindText();
        await WaitAsync("kynxaTranscriptSearch.state().open", "search opens before the controlled continuous DOM updates");
        try
        {
            await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Emulation.setCPUThrottlingRate", "{\"rate\":4}");
            await EvalAsync<bool>("""
                (() => {
                  const body=document.querySelector('.message-body'), fragment=document.createDocumentFragment();
                  for(let i=0;i<8000;i++){const p=document.createElement('p');p.textContent='STRESS_MATCH '+i;fragment.append(p);}
                  body.replaceChildren(fragment);
                  const detached=document.createElement('div'),hidden=document.createElement('div');
                  detached.className=hidden.className='message-body';detached.textContent=hidden.textContent='STRESS_MATCH';
                  body.parentElement.append(detached,hidden);
                  window.__searchStress={ticks:0,countsDuringUpdates:[],finished:false,errors:[]};
                  window.__searchStressError=event=>__searchStress.errors.push(String(event.error||event.message));
                  window.addEventListener('error',__searchStressError);
                  const input=document.getElementById('transcript-search-input');input.value='STRESS_MATCH';input.dispatchEvent(new Event('input',{bubbles:true}));
                  kynxaTranscriptSearch.refresh(true);
                  window.__searchStressTimer=setInterval(()=>{
                    if(__searchStress.ticks===0){detached.remove();hidden.hidden=true;}
                    if(kynxaTranscriptSearch.state().count>0)__searchStress.countsDuringUpdates.push(kynxaTranscriptSearch.state().count);
                    const p=document.createElement('p');p.textContent='STRESS_MATCH update '+__searchStress.ticks;body.append(p);
                    kynxaTranscriptSearch.refresh();
                    if(++__searchStress.ticks===80){clearInterval(__searchStressTimer);__searchStress.finished=true;}
                  },16);
                  return true;
                })()
                """);
            await WaitAsync("__searchStress.finished && !kynxaTranscriptSearch.state().searching && kynxaTranscriptSearch.state().count===8080", "large search settles on all 8080 current matches after eighty continuous DOM updates");
            Check(await EvalAsync<bool>("__searchStress.countsDuringUpdates.length>0"), "large conversation publishes valid search results while updates are still arriving instead of restarting indefinitely");
            Check(await EvalAsync<bool>("[...CSS.highlights.values()].every(highlight=>[...highlight].every(range=>range.startContainer.isConnected && range.endContainer.isConnected && !range.startContainer.parentElement.closest('[hidden]') && range.toString()==='STRESS_MATCH'))"), "coalesced continuous searches publish only connected visible exact literal ranges");
            Check(await EvalAsync<bool>("__searchStress.errors.length===0"), "removing or hiding captured bodies during continuous indexing never strands search or raises a page error");
            _metrics["bodySearchContinuousIndex"] = await EvalAsync<JsonElement>("({ticks:__searchStress.ticks,firstPublishedCount:__searchStress.countsDuringUpdates[0],updatesWithPublishedCount:__searchStress.countsDuringUpdates.length,final:kynxaTranscriptSearch.state()})");
        }
        finally
        {
            await EvalAsync<bool>("(() => {clearInterval(window.__searchStressTimer);window.removeEventListener('error',window.__searchStressError);return true;})()");
            await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Emulation.setCPUThrottlingRate", "{\"rate\":1}");
            _transcript.CloseFindText();
        }
    }

    private async Task CheckBodySearchBoundariesAsync()
    {
        var segmentedChat = Guid.NewGuid();
        var segmented = Message(segmentedChat, "assistant", "LEGACY_NOT_SEARCHABLE", "interrupted");
        segmented.Message.AssistantSegments =
        [
            new("search-segment-1", 1, 0, "commentary", "completed", "SAME_MATCH first segment", "HIDDEN_REASONING"),
            new("search-segment-2", 2, 2, "commentary", "interrupted", "SAME_MATCH second segment", "HIDDEN_REASONING")
        ];
        _transcript.ShowConversation(segmentedChat, [segmented]);
        await WaitAsync("document.querySelectorAll('.assistant-segment').length===2", "multiple visible assistant stages provide separate searchable bodies");
        _transcript.OpenFindText();
        await WaitAsync("kynxaTranscriptSearch.state().open", "search opens on segmented assistant content");
        await SetBodySearchQueryAsync("SAME_MATCH", 2);
        await ClickBodySearchAsync("previous");
        await WaitAsync("kynxaTranscriptSearch.state().index===1", "previous from an unselected list activates the last stage's match");
        var otherChat = Guid.NewGuid();
        _transcript.ShowConversation(otherChat, [Message(otherChat, "assistant", "BOUNDARY_OTHER")]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('BOUNDARY_OTHER')", "another conversation temporarily replaces segmented content");
        _transcript.ShowConversation(segmentedChat, [segmented]);
        await WaitAsync("kynxaTranscriptSearch.state().query==='SAME_MATCH' && !kynxaTranscriptSearch.state().searching", "segmented query is restored from the existing DOM cache");
        Check(await EvalAsync<int>("kynxaTranscriptSearch.state().index") == 1, "same-offset matches in different assistant bodies restore the correct active stage");
        await SetBodySearchQueryAsync("HIDDEN_REASONING", 0);
        await SetBodySearchQueryAsync("LEGACY_NOT_SEARCHABLE", 0);
        _transcript.CloseFindText();

        string fence = new((char)96, 3);
        var wideChat = Guid.NewGuid();
        string table = "| " + string.Join(" | ", Enumerable.Range(1, 20).Select(index => "Column" + index)) + " |\n| "
            + string.Join(" | ", Enumerable.Repeat("---", 20)) + " |\n| "
            + string.Join(" | ", Enumerable.Range(1, 20).Select(index => index == 20 ? "RIGHT_EDGE_MATCH" : "Cell" + index)) + " |";
        string markdown = "SEARCH_BOUNDARIES\n\n" + table + "\n\n" + fence
            + "mermaid\nflowchart LR\nA[\"BROKEN_SOURCE_MATCH\nA --> B\n" + fence + "\n\n" + fence
            + "mermaid\nflowchart LR\nA[\"MODAL_SOURCE_MATCH\"] --> B[\"Done\"]\n" + fence;
        _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(480, 850));
        var answer = Message(wideChat, "assistant", markdown);
        _transcript.ShowConversation(wideChat, [answer]);
        await WaitAsync("document.querySelectorAll('.mermaid-block').length===2 && !!document.querySelector('.mermaid-viewport svg')", "wide table and broken/valid Mermaid fixtures render independently");
        _transcript.OpenFindText();
        await WaitAsync("kynxaTranscriptSearch.state().open", "search opens in a narrow mixed-content conversation");
        await SetBodySearchQueryAsync("RIGHT_EDGE_MATCH", 1);
        Check(await EvalAsync<bool>("document.querySelector('.table-scroll').scrollWidth>document.querySelector('.table-scroll').clientWidth*2"), "far-right match begins outside a genuinely horizontally overflowing table");
        await ClickBodySearchAsync("next");
        await Task.Delay(150);
        Check(await EvalAsync<bool>("(() => {const scroll=document.querySelector('.table-scroll');const cell=scroll.querySelector('tbody tr').lastElementChild;const rect=cell.getBoundingClientRect(),bounds=scroll.getBoundingClientRect();return scroll.scrollLeft>100 && rect.left>=bounds.left-1 && rect.left<bounds.right && rect.top>=document.getElementById('transcript-search').getBoundingClientRect().bottom;})()"), "explicit table navigation reveals the rightmost cell inside its own horizontal scroller");
        await SetBodySearchQueryAsync("BROKEN_SOURCE_MATCH", 1);
        await ClickBodySearchAsync("next");
        await WaitAsync("document.querySelector('.mermaid-block .mermaid-source-details').open && kynxaTranscriptSearch.state().index===0", "render failure retains searchable original Mermaid source");
        await EvalAsync<bool>("(() => {document.querySelectorAll('.mermaid-block')[1].querySelector('[data-diagram-action=fullscreen]').click();return true;})()");
        await WaitAsync("!!document.querySelector('.mermaid-viewer[open]')", "actual Mermaid modal viewer opens above the current search bar");
        await EvalAsync<bool>("(() => {window.__searchModalQuery=kynxaTranscriptSearch.state().query;window.__searchModalOpen=kynxaTranscriptSearch.state().open;window.dispatchEvent(new KeyboardEvent('keydown',{key:'f',ctrlKey:true,bubbles:true,cancelable:true}));return true;})()");
        _transcript.OpenFindText();
        await Task.Delay(150);
        Check(await EvalAsync<bool>("!!document.querySelector('.mermaid-viewer[open]') && kynxaTranscriptSearch.state().query===__searchModalQuery && kynxaTranscriptSearch.state().open===__searchModalOpen && document.activeElement.id!=='transcript-search-input'"), "Ctrl+F and native open requests do not steal focus from the full-screen diagram modal");
        await EvalAsync<bool>("(() => {window.kynxaDiagrams.closeViewer();return true;})()");
        await WaitAsync("!document.querySelector('.mermaid-viewer[open]')", "diagram viewer closes without modifying the search query");
        await EvalAsync<bool>("(() => {document.getElementById('transcript-search-input').dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',isComposing:true,bubbles:true,cancelable:true}));return true;})()");
        Check(await EvalAsync<bool>("kynxaTranscriptSearch.state().open"), "composing Escape does not dismiss the search bar");
        _transcript.CloseFindText();

        _transcript.ShowConversation(null, []);
        await WaitAsync("transcriptState().messageCount===0", "empty conversation clears the previous indexed content");
        _transcript.OpenFindText();
        await WaitAsync("kynxaTranscriptSearch.state().open", "host can open find in a conversation without an identity or messages");
        await SetBodySearchQueryAsync("SAME_MATCH", 0);
        Check(await EvalAsync<bool>("!kynxaTranscriptSearch.state().conversationId && CSS.highlights.size<=1"), "empty conversation never retains earlier result ranges");
        _transcript.CloseFindText();

        for (int index = 0; index < 6; index++)
        {
            var id = Guid.NewGuid();
            _transcript.ShowConversation(id, [Message(id, "assistant", "CACHE_EVICTION_" + index)]);
            await WaitAsync("__transcriptSmoke.bodyText().includes('CACHE_EVICTION_" + index + "')", "cache eviction fixture switches to conversation " + index);
        }
        _transcript.ShowConversation(segmentedChat, [segmented]);
        await WaitAsync("document.querySelectorAll('.assistant-segment').length===2", "evicted conversation rebuilds from its original messages");
        Check(await EvalAsync<bool>("!kynxaTranscriptSearch.state().open && kynxaTranscriptSearch.state().query==='' && kynxaTranscriptSearch.state().count===0 && transcriptState().cachedConversations<=4"), "search state is bounded by the original four-conversation DOM cache and is discarded on eviction");
    }

    private async Task CheckBodySearchLayoutAsync()
    {
        var chat = Guid.NewGuid();
        var answer = Message(chat, "assistant", "SEARCH_LAYOUT 中文English\n\nİ marker I i 中文English");
        _transcript.ShowConversation(chat, [answer]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('SEARCH_LAYOUT')", "layout and Unicode search fixture is visible");
        _transcript.OpenFindText();
        await WaitAsync("kynxaTranscriptSearch.state().open", "layout search bar opens");
        await SetBodySearchQueryAsync("marker", 1);
        Check(await EvalAsync<bool>("[...CSS.highlights.values()].some(highlight=>[...highlight].some(range=>range.toString()==='marker'))"), "case normalization before a Unicode dotted-I does not shift highlight offsets");
        foreach (string language in new[] { "zh-CN", "en" })
        {
            UiText.Initialize(language);
            foreach (int width in new[] { 480, 980, 1600 })
            {
                _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(width, 850));
                await Task.Delay(200);
                Check(await EvalAsync<bool>("(() => {const bar=document.getElementById('transcript-search');const bounds=bar.getBoundingClientRect();return bounds.left>=0 && bounds.right<=innerWidth+1 && bounds.top>=0 && [...bar.querySelectorAll('input,button')].every(element=>{const rect=element.getBoundingClientRect();return rect.width>0 && rect.left>=bounds.left-1 && rect.right<=bounds.right+1;}) && document.documentElement.scrollWidth<=innerWidth;})()"), language + " search controls fit the actual native viewport at " + width);
                Check(await EvalAsync<bool>("kynxaTranscriptSearch.state().query==='marker' && kynxaTranscriptSearch.state().count===1"), language + " resizing retains the current query and result count at " + width);
                await ClickBodySearchAsync("next");
                await Task.Delay(100);
                Check(await EvalAsync<bool>("kynxaTranscriptSearch.state().position==='top' && document.getElementById('transcript-search').getBoundingClientRect().top===8"), language + " the search bar remains at the top after navigating a short first-screen match at " + width);
                Check(await EvalAsync<bool>("(() => {const highlight=CSS.highlights.get('transcript-search-current');if(!highlight)return false;const range=[...highlight][0].getBoundingClientRect(),bar=document.getElementById('transcript-search').getBoundingClientRect();return range.top>=0 && range.bottom<=innerHeight && (range.bottom<=bar.top || range.top>=bar.bottom || range.right<=bar.left || range.left>=bar.right);})()"), language + " explicit navigation keeps a short conversation's top match outside the floating bar at " + width);
                if (width == 480) await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), "kynxa-transcript-body-search-" + language + "-480.png"));
            }
        }
        Check(await EvalAsync<bool>("document.getElementById('transcript-search-input').placeholder.toLowerCase().includes('find') || document.getElementById('transcript-search-input').placeholder.toLowerCase().includes('search')"), "English search input is localized through the native language bridge");
        AppearanceService.Apply("teal");
        await Task.Delay(150);
        Check(await EvalAsync<bool>("kynxaTranscriptSearch.state().query==='marker' && kynxaTranscriptSearch.state().count===1 && getComputedStyle(document.getElementById('transcript-search')).fontFamily.includes('Segoe')"), "theme changes retain search state and use the shared application typography");
        AppearanceService.ApplyPreferences("teal", "dark", 22, "#A568C2");
        await Task.Delay(200);
        Check(await EvalAsync<bool>("kynxaTranscriptSearch.state().query==='marker' && kynxaTranscriptSearch.state().count===1 && getComputedStyle(document.documentElement).getPropertyValue('--chat-font-size').trim()==='22px'"), "dark mode, custom accent and largest reading font retain the search query and count");
        await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), "kynxa-transcript-body-search.png"));
        await EvalAsync<bool>("(() => {window.__shortSearchReading={inset:kynxaTranscriptSearch.state().topInsetPx,top:[...CSS.highlights.get('transcript-search-current')][0].getBoundingClientRect().top};return true;})()");
        var otherChat = Guid.NewGuid();
        _transcript.ShowConversation(otherChat, [Message(otherChat, "assistant", "SHORT_SEARCH_OTHER")]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('SHORT_SEARCH_OTHER') && !kynxaTranscriptSearch.state().open && !document.getElementById('messages').style.paddingTop", "first-screen clearance never leaks into another conversation");
        _transcript.ShowConversation(chat, [answer]);
        await WaitAsync("kynxaTranscriptSearch.state().open && kynxaTranscriptSearch.state().query==='marker' && kynxaTranscriptSearch.state().index===0 && !kynxaTranscriptSearch.state().searching", "returning to the short conversation restores its find result without another navigation");
        Check(await EvalAsync<bool>("kynxaTranscriptSearch.state().topInsetPx===__shortSearchReading.inset && Math.abs([...CSS.highlights.get('transcript-search-current')][0].getBoundingClientRect().top-__shortSearchReading.top)<2"), "cached first-screen clearance retains the exact visible reading position");
        _transcript.CloseFindText();
        await WaitAsync("!kynxaTranscriptSearch.state().open && !document.getElementById('messages').style.paddingTop", "closing search removes only the temporary first-match clearance");
    }
}
