using System.Text.Json;
using KYNXA_Desktop.Services;

namespace TranscriptUiSmoke;

public partial class App
{
    private async Task CheckReplyTimingAsync()
    {
        var chat = Guid.NewGuid();
        var user = Message(chat, "user", "TIMING_USER_UNCHANGED");
        var reply = Message(chat, "assistant", "", "streaming");
        _transcript.ShowConversation(chat, [user, reply]);
        await WaitAsync("document.querySelectorAll('#messages > article').length === 2", "waiting reply renders before generation starts");
        Check(await EvalAsync<bool>("document.querySelector('.assistant .message-elapsed').hidden && transcriptState().liveElapsedCount === 0 && !transcriptState().elapsedTimerActive"), "preparation shows no elapsed clock or interval");
        Check(await EvalAsync<bool>("KynxaMessagePresentation.elapsedText(0, {}, {live:true}) === '用时 0秒' && KynxaMessagePresentation.elapsedText(0, {}, {known:true}) === '用时 0秒' && KynxaMessagePresentation.elapsedText(0, {}) === '' && KynxaMessagePresentation.elapsedText(-1, {}, {live:true}) === ''"), "known zero is visible while unknown legacy final duration stays hidden");

        // Exercise the real host monotonic timestamp rather than setting a synthetic browser clock.
        // 使用宿主真实单调时间戳验证完整链路，不通过伪造浏览器时钟绕过计时逻辑。
        reply.Message.StartGenerationTiming();
        reply.Message.ToolActivities =
        [
            new("timing-tool", "filesystem.read", JsonSerializer.SerializeToElement(new { path = "timing.txt" }), "completed", "TIMING_TOOL_ONLY")
        ];
        reply.Refresh();
        await WaitAsync("document.querySelector('.assistant .message-elapsed').dataset.mode === 'live' && !!document.querySelector('[data-tool-call-id=timing-tool]')", "a first tool-only round displays its live clock before prose");
        reply.Message.AssistantSegments =
        [
            new("timing-first", 1, 0, "commentary", "completed", "TIMING_FIRST\n\n**Stable prose**", ""),
            new("timing-last", 2, 2, "commentary", "streaming", "TIMING_DRAFT", "")
        ];
        reply.Refresh();
        await WaitAsync("!!document.querySelector('[data-segment-id=timing-first] .message-body')?.firstChild && document.querySelector('[data-segment-id=timing-last] .message-body')?.textContent.includes('TIMING_DRAFT') === true && document.querySelector('.assistant .message-elapsed').dataset.mode === 'live' && transcriptState().elapsedTimerActive", "prepared model prose arrives without resetting the first tool-only clock");
        Check(await EvalAsync<bool>("!document.querySelector('.assistant .message-elapsed').hidden && document.querySelector('.assistant .message-elapsed').textContent.includes('0') && transcriptState().liveElapsedCount === 1"), "generation starts at visible zero seconds with one active clock");
        Check(await EvalAsync<bool>("!!(document.querySelector('.assistant .message-elapsed').compareDocumentPosition(document.querySelector('.assistant .assistant-timeline')) & Node.DOCUMENT_POSITION_FOLLOWING) && document.querySelector('.assistant .message-elapsed').hasAttribute('data-copy-ignore')"), "elapsed control sits above reply prose and outside copy");
        await EvalAsync<bool>("(() => { window.__timingStableBody = document.querySelector('[data-segment-id=timing-first] .message-body').firstChild; return true; })()");
        await WaitAsync("!document.querySelector('.assistant .message-elapsed').textContent.includes('0秒')", "live clock advances without another model event");
        Check(await EvalAsync<bool>("document.querySelector('[data-segment-id=timing-first] .message-body').firstChild === window.__timingStableBody"), "timer ticks preserve the rendered Markdown node");

        reply.Message.AssistantSegments[^1] = reply.Message.AssistantSegments[^1] with { Content = "TIMING_DRAFT_MORE" };
        reply.Refresh();
        await WaitAsync("document.querySelector('[data-segment-id=timing-last]').textContent.includes('TIMING_DRAFT_MORE')", "streaming token update reaches the active stage");
        Check(await EvalAsync<bool>("!document.querySelector('.assistant .message-elapsed').textContent.includes('0秒') && document.querySelector('[data-segment-id=timing-first] .message-body').firstChild === window.__timingStableBody"), "token updates retain the current attempt clock and earlier prose");
        string language = UiText.Language;
        try
        {
            UiText.Initialize("en");
            await WaitAsync("document.documentElement.lang === 'en' && !/[\\u3400-\\u9fff]/u.test(document.querySelector('.assistant .message-elapsed').textContent)", "live elapsed label follows interface language immediately");
            Check(await EvalAsync<bool>("document.querySelector('[data-segment-id=timing-first] .message-body').firstChild === window.__timingStableBody && document.querySelector('.user .message-body').textContent === 'TIMING_USER_UNCHANGED'"), "live translation leaves assistant and user content untouched");
        }
        finally { UiText.Initialize(language); }
        await WaitAsync("document.documentElement.lang === 'zh-CN'", "timing fixture returns to Chinese interface strings");

        _transcript.ShowConversation(Guid.NewGuid(), []);
        await WaitAsync("!document.querySelector('#messages > article') && transcriptState().liveElapsedCount === 0 && !transcriptState().elapsedTimerActive", "switching away stops the visible interval");
        await Task.Delay(1100);
        _transcript.ShowConversation(chat, [user, reply]);
        await WaitAsync("transcriptState().liveElapsedCount === 1 && transcriptState().elapsedTimerActive", "cached live conversation resumes its existing clock");
        Check(await EvalAsync<bool>("document.querySelector('[data-segment-id=timing-first] .message-body').firstChild === window.__timingStableBody && !document.querySelector('.assistant .message-elapsed').textContent.includes('0秒')"), "chat navigation reuses prose and never restarts elapsed time");

        reply.Message.ApplyGenerationTiming(reply.Message.GenerationElapsedMs!.Value, false);
        reply.Refresh();
        await WaitAsync("document.querySelector('.assistant .message-elapsed').dataset.mode === 'paused' && !document.querySelector('.assistant .message-elapsed').hidden && !transcriptState().elapsedTimerActive && transcriptState().liveElapsedCount === 0", "pausing freezes the known duration without hiding it");
        await EvalAsync<bool>("(() => { window.__pausedTimingLabel = document.querySelector('.assistant .message-elapsed').textContent; return true; })()");
        await Task.Delay(1100);
        Check(await EvalAsync<bool>("document.querySelector('.assistant .message-elapsed').textContent === window.__pausedTimingLabel"), "a paused clock remains visible and does not keep accumulating");
        reply.Message.StartGenerationTiming();
        reply.Refresh();
        await WaitAsync("document.querySelector('.assistant .message-elapsed').dataset.mode === 'live' && transcriptState().elapsedTimerActive", "the same task resumes its retained duration after a pause");

        await EvalAsync<bool>("(() => { const range = document.createRange(); range.selectNodeContents(window.__timingStableBody); getSelection().removeAllRanges(); getSelection().addRange(range); return true; })()");
        reply.Message.AssistantSegments[^1] = reply.Message.AssistantSegments[^1] with
        { Phase = "final_answer", Status = "completed", Content = "TIMING_FINAL_VERIFIED" };
        reply.Message.Content = "TIMING_FINAL_VERIFIED";
        reply.Message.Status = "completed";
        reply.Message.DurationMs = 0;
        reply.Refresh();
        await WaitAsync("transcriptState().pending && !transcriptState().elapsedTimerActive && !document.querySelector('.assistant .message-elapsed').hidden", "final prose arriving before its canonical total keeps the known frozen clock outside a selection");
        reply.Message.DurationMs = 3200;
        reply.Refresh();
        await WaitAsync("transcriptState().pending && !transcriptState().elapsedTimerActive && transcriptState().liveElapsedCount === 0 && document.querySelector('.assistant .message-elapsed').textContent === '用时 4秒'", "the canonical final total arrives even while a selection defers body convergence");
        Check(await EvalAsync<bool>("getSelection().toString() === 'TIMING_FIRST' && document.querySelector('[data-segment-id=timing-first] .message-body').firstChild === window.__timingStableBody && document.querySelectorAll('.assistant-segment').length === 2"), "terminal timing update does not replace selected intermediate prose");
        Check(await EvalAsync<bool>("document.querySelector('.assistant .message-elapsed').dataset.mode === 'final' && document.querySelector('.assistant .message-elapsed').textContent === '用时 4秒'"), "successful final duration comes from the canonical gateway total");
        await Task.Delay(1100);
        Check(await EvalAsync<bool>("document.querySelector('.assistant .message-elapsed').textContent === '用时 4秒' && !transcriptState().elapsedTimerActive && getSelection().toString() === 'TIMING_FIRST'"), "completed selected reply cannot keep ticking after another second");
        _transcript.ClearSelection();
        await WaitAsync("!transcriptState().pending && document.querySelectorAll('.assistant-segment').length === 1 && document.querySelector('.final-answer').textContent.includes('TIMING_FINAL_VERIFIED')", "clearing selection applies the preserved final answer");
        Check(await EvalAsync<bool>("document.querySelector('.assistant .message-elapsed').textContent === '用时 4秒' && !transcriptState().elapsedTimerActive"), "final convergence keeps the backend duration without another live interval");
        string persisted = JsonSerializer.Serialize(reply.Message);
        Check(!persisted.Contains("GenerationStartedTimestamp", StringComparison.OrdinalIgnoreCase)
            && JsonSerializer.Deserialize<KYNXA_Desktop.Models.UI.ChatMessageState>(persisted)?.GenerationStartedTimestamp is null,
            "process-local timing origin is never persisted in chat data");

        // Retrying reuses the reply identity, but must start a new generation attempt.
        // 重试复用回复 ID，但必须创建新的生成尝试，不能继续上一次用时。
        var replyId = reply.Message.Id;
        reply = Message(chat, "assistant", "", "streaming");
        reply.Message.Id = replyId;
        reply.Message.DurationMs = 3200;
        _transcript.ShowConversation(chat, [user, reply]);
        await WaitAsync("document.querySelector('.assistant').dataset.presentationMode === 'active' && document.querySelector('.assistant .message-elapsed').hidden", "same-ID retry preparation clears the old elapsed state");
        reply.Message.DurationMs = 0;
        reply.Message.StartGenerationTiming();
        reply.Message.Content = "RETRY_RUNNING";
        reply.Refresh();
        await WaitAsync("document.querySelector('.assistant .message-elapsed').dataset.mode === 'live'", "same-ID retry starts from its new host timestamp");
        Check(await EvalAsync<bool>("document.querySelector('.assistant .message-elapsed').textContent === '用时 0秒' && transcriptState().liveElapsedCount === 1"), "retry duration starts at zero instead of the previous attempt total");
        reply.Message.Status = "interrupted";
        reply.Refresh();
        await WaitAsync("!document.querySelector('.assistant .message-elapsed').hidden && !transcriptState().elapsedTimerActive && transcriptState().liveElapsedCount === 0", "cancellation stops the interval while preserving its known duration");

        // Cover the coalesced retry path where a waiting snapshot never reaches the browser.
        // 覆盖等待快照被合并、未到达浏览器的重试路径；结束状态也必须重置下一次计时。
        reply = Message(chat, "assistant", "COALESCED_RETRY_RUNNING", "streaming");
        reply.Message.Id = replyId;
        reply.Message.StartGenerationTiming();
        _transcript.ShowConversation(chat, [user, reply]);
        await WaitAsync("document.querySelector('.assistant .message-elapsed').dataset.mode === 'live'", "direct interrupted-to-streaming transition begins a fresh attempt");
        Check(await EvalAsync<bool>("document.querySelector('.assistant .message-elapsed').textContent === '用时 0秒'"), "coalesced retry cannot reuse an older clock anchor");
        reply.Message.Status = "error";
        reply.Message.Error = "TIMING_TEST_FAILURE";
        reply.Refresh();
        await WaitAsync("document.querySelector('.assistant .message-status').textContent.includes('TIMING_TEST_FAILURE') && !transcriptState().elapsedTimerActive && !document.querySelector('.assistant .message-elapsed').hidden", "error freezes the visible duration and retains its failure state");

        // A new attempt may begin while the old live DOM is cached and never receive a reset frame.
        // 旧活动 DOM 缓存期间可能已开始新的重试，返回时不能依赖中间重置快照清掉旧起点。
        reply = Message(chat, "assistant", "CACHED_OLD_ATTEMPT", "streaming");
        reply.Message.Id = replyId;
        reply.Message.ApplyGenerationTiming(5000, true);
        _transcript.ShowConversation(chat, [user, reply]);
        await WaitAsync("document.querySelector('.assistant .message-elapsed').dataset.mode === 'live' && !document.querySelector('.assistant .message-elapsed').textContent.includes('0秒') && transcriptState().elapsedTimerActive", "the cached retry fixture starts with an established older clock");
        _transcript.ShowConversation(Guid.NewGuid(), []);
        await WaitAsync("!document.querySelector('#messages > article') && !transcriptState().elapsedTimerActive", "an inactive conversation retains its DOM without an active interval");
        reply.Message.Status = "error";
        reply = Message(chat, "assistant", "CACHED_NEW_ATTEMPT", "streaming");
        reply.Message.Id = replyId;
        reply.Message.StartGenerationTiming();
        _transcript.ShowConversation(chat, [user, reply]);
        await WaitAsync("document.querySelector('.assistant .message-body').textContent.includes('CACHED_NEW_ATTEMPT') && document.querySelector('.assistant .message-elapsed').dataset.mode === 'live'", "returning to the cached conversation applies the new attempt identity");
        Check(await EvalAsync<bool>("document.querySelector('.assistant .message-elapsed').textContent === '用时 0秒' && transcriptState().liveElapsedCount === 1"), "a cached same-message retry cannot inherit its previous clock origin");
        reply = Message(chat, "assistant", "PAUSED_KNOWN_ZERO", "streaming");
        reply.Message.Id = replyId;
        reply.Message.ApplyGenerationTiming(0, false);
        _transcript.ShowConversation(chat, [user, reply]);
        await WaitAsync("document.querySelector('.assistant .message-elapsed').dataset.mode === 'paused' && document.querySelector('.assistant .message-elapsed').textContent === '用时 0秒' && !transcriptState().elapsedTimerActive", "a trusted paused zero remains visible without a running interval");

        // Later ordinary replies retain their own clock alongside the earlier settled result.
        // 普通连续回复各自计时，不能因首条已结算或同一聊天缓存而隐藏后一条计时。
        var previousReply = Message(chat, "assistant", "PREVIOUS_SETTLED_REPLY");
        previousReply.Message.DurationMs = 3200;
        var laterReply = Message(chat, "assistant", "", "streaming");
        _transcript.ShowConversation(chat, [user, previousReply, laterReply]);
        await WaitAsync("document.querySelectorAll('.assistant .message-elapsed').length === 2 && document.querySelectorAll('.assistant .message-elapsed')[0].dataset.mode === 'final' && document.querySelectorAll('.assistant .message-elapsed')[1].hidden", "the next ordinary reply prepares independently of the previous reply");
        laterReply.Message.ApplyGenerationTiming(2300, true);
        laterReply.Refresh();
        await WaitAsync("document.querySelectorAll('.assistant .message-elapsed')[0].dataset.mode === 'final' && document.querySelectorAll('.assistant .message-elapsed')[1].dataset.mode === 'live' && !document.querySelectorAll('.assistant .message-elapsed')[1].hidden && transcriptState().liveElapsedCount === 1", "a later tool-only reply shows its own live clock while the first reply retains its total");

        await EvalAsync<bool>("""
            (() => {
              window.__legacyTimingBase = {id:'legacy-timing-reply',role:'assistant',
                content:'LEGACY_TIMING',html:'<p>LEGACY_TIMING</p>',status:'streaming',streaming:true,
                durationMs:0,generationElapsedMs:5000,toolActivities:[],assistantSegments:[]};
              applyTranscript({conversationId:'legacy-timing-compatibility',messages:[__legacyTimingBase]});
              return true;
            })()
            """);
        await WaitAsync("document.querySelector('.assistant')?.dataset.messageId === 'legacy-timing-reply' && document.querySelector('.assistant .message-elapsed')?.dataset.mode === 'live'", "an older snapshot can establish a live clock without the new timing contract");
        Check(await EvalAsync<bool>("""
            (() => {
              applyTranscript({conversationId:'legacy-timing-compatibility',messages:[{...__legacyTimingBase,
                status:'completed',streaming:false,generationElapsedMs:null}]});
              return !document.querySelector('.assistant .message-elapsed').hidden
                && document.querySelector('.assistant .message-elapsed').dataset.mode === 'final'
                && !transcriptState().elapsedTimerActive;
            })()
            """), "an older final snapshot without a total preserves its established clock");
    }
}
