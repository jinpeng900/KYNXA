using System.Text.Json;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.ViewModels;

namespace TranscriptUiSmoke;

public partial class App
{
    // Exercise local reading features across the current native transcript's real recycling boundary.
    // 跨当前原生聊天的真实 DOM 回收边界验收本地阅读功能，不复制虚拟化或搜索实现。
    private async Task CheckNativeMergeAsync()
    {
        _transcript.CloseFindText();
        _transcript.ClearSelection();
        var chatA = Guid.NewGuid();
        var chatB = Guid.NewGuid();
        string fence = new((char)96, 3);
        var rowsA = Enumerable.Range(0, 64).Select(index => Message(chatA, "assistant",
            $"NATIVE_LOADED_MATCH_{index:00}\n\n" + string.Join("\n\n", Enumerable.Range(0, 8)
                .Select(paragraph => $"ROW_{index:00}_PARAGRAPH_{paragraph:00} 中文 English reading position."))
            + (index == 12 ? "\n\n" + fence + "txt\n" + string.Join("\n", Enumerable.Range(1, 30)
                .Select(line => $"NATIVE_CODE_{line:00} " + new string('x', 160))) + "\n" + fence : ""))).ToList();
        var rowsB = Enumerable.Range(0, 64).Select(index => Message(chatB, "assistant",
            $"NATIVE_OTHER_{index:00}\n\n" + string.Join("\n\n", Enumerable.Range(0, 8)
                .Select(paragraph => $"OTHER_{index:00}_PARAGRAPH_{paragraph:00} 另一个会话。")))).ToList();
        string firstArticle = NativeMergeArticle(rowsA[0]);
        string codeArticle = NativeMergeArticle(rowsA[12]);
        string anchorArticle = NativeMergeArticle(rowsA[30]);
        int checksBefore = _checks;
        try
        {
            _transcript.ShowConversation(chatA, rowsA);
            await WaitAsync("transcriptState().conversationId===" + JsonSerializer.Serialize(chatA.ToString())
                + " && transcriptState().messageCount===64 && __transcriptSmoke.bottomDistance()<4",
                "64 loaded messages open at the latest content");
            await WaitAsync("transcriptState().mountedMessageCount<64 && document.querySelectorAll('article[data-virtualized]').length>0 && "
                + firstArticle + ".dataset.virtualized==='true'", "distant loaded messages are genuinely virtualized before search");
            _metrics["nativeMergeMountedBeforeSearch"] = await EvalAsync<int>("transcriptState().mountedMessageCount");
            await CheckNativeMergeSmallUpwardStepAsync();

            _transcript.OpenFindText();
            await WaitAsync("kynxaTranscriptSearch.state().open", "native search opens in a virtualized conversation");
            await SetBodySearchQueryAsync("NATIVE_LOADED_MATCH", 64);
            Check(await EvalAsync<bool>("transcriptState().mountedMessageCount===64 && document.querySelectorAll('article[data-virtualized]').length===0"),
                "search includes all currently loaded messages, including distant recycled bodies");
            await SetBodySearchQueryAsync("NATIVE_LOADED_MATCH_00", 1);
            await ClickBodySearchAsync("next");
            // Topmost matches can move the floating search bar below the text; assert the actual highlighted range is unobscured on either side.
            // 顶部匹配会让搜索浮栏移到正文下方；验收实际高亮范围在浮栏任一侧完整可见，不假定浮栏始终在顶部。
            await WaitAsync("kynxaTranscriptSearch.state().index===0 && (()=>{const highlight=CSS.highlights.get('transcript-search-current');if(!highlight)return false;const range=[...highlight][0];if(!range || range.toString()!=='NATIVE_LOADED_MATCH_00' || !"
                + firstArticle + ".contains(range.startContainer))return false;const bounds=range.getBoundingClientRect(),bar=document.getElementById('transcript-search').getBoundingClientRect();return bounds.width>0 && bounds.height>0 && bounds.left>=0 && bounds.right<=innerWidth && bounds.top>=0 && bounds.bottom<=innerHeight && (bounds.bottom<=bar.top || bounds.top>=bar.bottom || bounds.right<=bar.left || bounds.left>=bar.right);})()",
                "explicit search navigation reveals a match from the previously unmounted first message");
            _metrics["nativeMergeFirstSearchNavigation"] = await EvalAsync<JsonElement>("(()=>{const geometry=rect=>({top:rect.top,bottom:rect.bottom,left:rect.left,right:rect.right,width:rect.width,height:rect.height});return {state:kynxaTranscriptSearch.state(),range:geometry([...CSS.highlights.get('transcript-search-current')][0].getBoundingClientRect()),bar:geometry(document.getElementById('transcript-search').getBoundingClientRect()),viewport:{width:innerWidth,height:innerHeight}};})()");
            _transcript.CloseFindText();
            await WaitAsync("!kynxaTranscriptSearch.state().open && transcriptState().mountedMessageCount<64 && document.querySelectorAll('article[data-virtualized]').length>0",
                "closing search restores the bounded body DOM window");

            await ScrollNativeMergeToArticleAsync(codeArticle, "FirstCode");
            await WaitAsync("!transcriptState().following && !!" + codeArticle + "?.querySelector('.content-block [data-block-action=fold]')",
                "scrolling to a distant code message mounts its production reading controls");
            await EvalAsync<bool>("(()=>{const block=" + codeArticle + ".querySelector('.content-block');window.__nativeMergeOldBlock=block;"
                + "block.querySelector('[data-block-action=wrap]').click();block.querySelector('[data-block-action=fold]').click();return true;})()");
            await WaitAsync(codeArticle + ".querySelector('.content-block')?.dataset.wrap==='true' && "
                + codeArticle + ".querySelector('.content-block')?.dataset.collapsed==='true'",
                "a reader explicitly enables wrapping and the twelve-line preview");
            await EvalAsync<bool>("(()=>{scrollTo({top:document.documentElement.scrollHeight,behavior:'instant'});return true;})()");
            await WaitAsync(codeArticle + ".dataset.virtualized==='true' && !__nativeMergeOldBlock.isConnected",
                "scrolling away releases the code message and its old block DOM");
            await ScrollNativeMergeToArticleAsync(codeArticle, "RecycledCode");
            await WaitAsync("!transcriptState().following && (()=>{const block=" + codeArticle + ".querySelector('.content-block');return !!block && block!==__nativeMergeOldBlock && block.dataset.wrap==='true' && block.dataset.collapsed==='true';})()",
                "newly mounted code controls restore the reader's wrap and fold preferences");

            await ScrollNativeMergeToArticleAsync(anchorArticle, "Anchor");
            await WaitAsync("!transcriptState().following && !!" + anchorArticle + "?.querySelector('.message-body > p')", "the long-conversation reading anchor is mounted");
            await EvalAsync<bool>("(()=>{const anchor=" + anchorArticle + ".querySelector('.message-body > p');anchor.scrollIntoView({block:'center',behavior:'instant'});return true;})()");
            await Task.Delay(180);
            await EvalAsync<bool>("(()=>{window.__nativeMergeAnchorTop=" + anchorArticle + ".querySelector('.message-body > p').getBoundingClientRect().top;return true;})()");
            Check(await EvalAsync<bool>("!transcriptState().following && __transcriptSmoke.bottomDistance()>300"),
                "the saved reading anchor is above the latest content with following paused");
            _transcript.ShowConversation(chatB, rowsB);
            await WaitAsync("transcriptState().conversationId===" + JsonSerializer.Serialize(chatB.ToString())
                + " && transcriptState().messageCount===64 && __transcriptSmoke.bottomDistance()<4 && transcriptState().mountedMessageCount<64",
                "another long conversation opens with its own virtualized latest window");
            _transcript.ShowConversation(chatA, rowsA);
            await WaitAsync("transcriptState().conversationId===" + JsonSerializer.Serialize(chatA.ToString())
                + " && !!" + anchorArticle + "?.querySelector('.message-body > p')", "returning to A restores its loaded reading region");
            await Task.Delay(180);
            Check(await EvalAsync<bool>("!transcriptState().following && Math.abs(" + anchorArticle
                + ".querySelector('.message-body > p').getBoundingClientRect().top-__nativeMergeAnchorTop)<3"),
                "A to B to A retains the visible paragraph position across virtualized long conversations");
            _metrics["nativeMergeReturnedAnchor"] = await EvalAsync<JsonElement>("({expected:__nativeMergeAnchorTop,actual:"
                + anchorArticle + ".querySelector('.message-body > p').getBoundingClientRect().top,state:transcriptState()})");
            await ScrollNativeMergeToArticleAsync(codeArticle, "CachedCode");
            await WaitAsync("!transcriptState().following && " + codeArticle + ".querySelector('.content-block')?.dataset.wrap==='true' && "
                + codeArticle + ".querySelector('.content-block')?.dataset.collapsed==='true'",
                "cached conversation restoration retains previously recycled code preferences");

            await CheckNativeMergeHistoryAsync(chatA);
            Check(rowsA.All(row => row.Message.Content.Contains("NATIVE_LOADED_MATCH_", StringComparison.Ordinal))
                && rowsA[12].Message.Content.Contains(new string('x', 160), StringComparison.Ordinal),
                "virtualization, search and reading controls leave authoritative synthetic source messages intact");
            _metrics["nativeMergeChecks"] = _checks - checksBefore;
        }
        finally
        {
            _transcript.CloseFindText();
            _transcript.ClearSelection();
            _transcript.SetHistoryAvailability(chatA, false, false);
        }
    }

    // Preserve the first small upward frame inside the near-bottom threshold, then test explicit follow resumption.
    // 验证底部阈值内首个微小上滚帧不会被自动跟随拉回，再检查显式跳转可以恢复跟随。
    private async Task CheckNativeMergeSmallUpwardStepAsync()
    {
        await WaitAsync("transcriptState().following && __transcriptSmoke.bottomDistance()<1",
            "the virtualized conversation settles at the bottom before a small upward step");
        await EvalAsync<bool>("(()=>{window.__nativeMergeSmallStepSettled=false;requestAnimationFrame(()=>requestAnimationFrame(()=>{const top=document.documentElement.scrollHeight-innerHeight-6;window.dispatchEvent(new WheelEvent('wheel',{deltaY:-100,bubbles:true}));scrollTo({top,behavior:'instant'});window.__nativeMergeSmallStepTop=top;requestAnimationFrame(()=>requestAnimationFrame(()=>{window.__nativeMergeSmallStepSettled=true;}));}));return true;})()");
        await WaitAsync("__nativeMergeSmallStepSettled", "the small upward frame passes through animation and viewport reconciliation");
        _metrics["nativeMergeSmallUpwardStep"] = await EvalAsync<JsonElement>("({state:transcriptState(),expectedTop:__nativeMergeSmallStepTop,actualTop:scrollY,bottomDistance:__transcriptSmoke.bottomDistance()})");
        Check(await EvalAsync<bool>("!transcriptState().following && __transcriptSmoke.bottomDistance()>0 && Math.abs(scrollY-__nativeMergeSmallStepTop)<1"),
            "an explicit small upward step stays paused above the bottom instead of being pulled back by the near-bottom threshold");
        _transcript.JumpToLatest();
        await WaitAsync("transcriptState().following && __transcriptSmoke.bottomDistance()<1",
            "explicit JumpToLatest restores following and reaches the actual bottom after a paused small step");
        _metrics["nativeMergeSmallStepResume"] = await EvalAsync<JsonElement>("({state:transcriptState(),top:scrollY,bottomDistance:__transcriptSmoke.bottomDistance()})");
    }

    // Invoke the production upward-input handler before positioning a recycled target with a synthetic viewport step.
    // 夹具通过合成上滚事件调用正式处理器，再定位已回收目标；不将程序滚动描述为操作系统原生输入。
    private async Task ScrollNativeMergeToArticleAsync(string article, string phase)
    {
        Check(await EvalAsync<bool>("(()=>{window.dispatchEvent(new WheelEvent('wheel',{deltaY:-120,bubbles:true}));if(transcriptState().following)return false;"
            + article + ".scrollIntoView({block:'center',behavior:'instant'});return true;})()"),
            phase + " upward input explicitly pauses bottom following before navigation");
        _metrics["nativeMergeNavigation" + phase] = await EvalAsync<JsonElement>("(()=>{const rect=" + article
            + ".getBoundingClientRect();return {state:transcriptState(),top:scrollY,bottomDistance:__transcriptSmoke.bottomDistance(),article:{top:rect.top,bottom:rect.bottom,height:rect.height},viewportHeight:innerHeight};})()");
    }

    private async Task CheckNativeMergeHistoryAsync(Guid conversationId)
    {
        var latest = new TaskCompletionSource<ConversationHistoryRequest>(TaskCreationOptions.RunContinuationsAsynchronously);
        var older = new TaskCompletionSource<Guid>(TaskCreationOptions.RunContinuationsAsynchronously);
        int olderRequests = 0;
        void NavigationRequested(object? sender, ConversationHistoryRequest request) => latest.TrySetResult(request);
        void HistoryRequested(object? sender, Guid requestedConversation)
        {
            if (requestedConversation != conversationId) return;
            olderRequests++;
            older.TrySetResult(requestedConversation);
        }
        _transcript.HistoryNavigationRequested += NavigationRequested;
        _transcript.HistoryRequested += HistoryRequested;
        try
        {
            await EvalAsync<bool>("(()=>{window.__nativeMergeHistoryAvailability=null;window.__nativeMergeHistoryListener=event=>{if(event.data?.type==='historyAvailability')window.__nativeMergeHistoryAvailability=event.data;};window.chrome.webview.addEventListener('message',__nativeMergeHistoryListener);return true;})()");
            _transcript.SetHistoryAvailability(conversationId, true, true);
            await WaitAsync("__nativeMergeHistoryAvailability?.conversationId===" + JsonSerializer.Serialize(conversationId.ToString())
                + " && __nativeMergeHistoryAvailability.hasOlder && __nativeMergeHistoryAvailability.hasNewer",
                "native page availability reaches the real WebView before navigation");
            await WaitAsync("!document.getElementById('jump-to-latest').hidden", "a loaded historical window exposes native latest-page navigation");
            await EvalAsync<bool>("(()=>{document.getElementById('jump-to-latest').click();return true;})()");
            var request = await latest.Task.WaitAsync(TimeSpan.FromSeconds(5));
            Check(request.ConversationId == conversationId && request.Direction == "latest",
                "latest button still reaches the host with the active conversation and native page direction");
            _transcript.SetHistoryAvailability(conversationId, true, false);
            await WaitAsync("__nativeMergeHistoryAvailability?.hasOlder && __nativeMergeHistoryAvailability.hasNewer===false",
                "the latest-page availability change settles before requesting older history");
            await EvalAsync<bool>("(()=>{scrollTo({top:0,behavior:'instant'});return true;})()");
            await WaitAsync("scrollY<1", "history fixture reaches the start of its loaded page");
            await EvalAsync<bool>("(()=>{window.dispatchEvent(new WheelEvent('wheel',{deltaY:-120,bubbles:true}));return true;})()");
            Check(await older.Task.WaitAsync(TimeSpan.FromSeconds(5)) == conversationId, "explicit upward input still requests the current conversation's older native page");
            await Task.Delay(180);
            Check(olderRequests == 1, "native history admission does not automatically walk through further pages");
        }
        finally
        {
            _transcript.HistoryNavigationRequested -= NavigationRequested;
            _transcript.HistoryRequested -= HistoryRequested;
            await EvalAsync<bool>("(()=>{if(window.__nativeMergeHistoryListener)window.chrome.webview.removeEventListener('message',__nativeMergeHistoryListener);delete window.__nativeMergeHistoryListener;delete window.__nativeMergeHistoryAvailability;return true;})()");
        }
    }

    private static string NativeMergeArticle(ConversationMessageViewModel row) =>
        "document.querySelector(" + JsonSerializer.Serialize($"article[data-message-id=\"{row.Message.Id}\"]") + ")";
}
