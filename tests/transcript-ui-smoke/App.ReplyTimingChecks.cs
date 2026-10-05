using System.Diagnostics;
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
        Check(await EvalAsync<bool>("KynxaMessagePresentation.elapsedText(0, {}, {live:true}) === '用时 0秒' && KynxaMessagePresentation.elapsedText(0, {}) === '' && KynxaMessagePresentation.elapsedText(-1, {}, {live:true}) === ''"), "live zero is visible while unknown legacy final duration stays hidden");

        // Exercise the real host monotonic timestamp rather than setting a synthetic browser clock.
        // 使用宿主真实单调时间戳验证完整链路，不通过伪造浏览器时钟绕过计时逻辑。
        reply.Message.GenerationStartedTimestamp = Stopwatch.GetTimestamp();
        reply.Message.AssistantSegments =
        [
            new("timing-first", 1, 0, "commentary", "completed", "TIMING_FIRST\n\n**Stable prose**", ""),
            new("timing-last", 2, 2, "commentary", "streaming", "TIMING_DRAFT", "")
        ];
        reply.Refresh();
        await WaitAsync("document.querySelector('.assistant .message-elapsed').dataset.mode === 'live' && transcriptState().elapsedTimerActive", "first prepared model segment starts a live duration");
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

        await EvalAsync<bool>("(() => { const range = document.createRange(); range.selectNodeContents(window.__timingStableBody); getSelection().removeAllRanges(); getSelection().addRange(range); return true; })()");
        reply.Message.AssistantSegments[^1] = reply.Message.AssistantSegments[^1] with
        { Phase = "final_answer", Status = "completed", Content = "TIMING_FINAL_VERIFIED" };
        reply.Message.Content = "TIMING_FINAL_VERIFIED";
        reply.Message.Status = "completed";
        reply.Message.DurationMs = 3200;
        reply.Refresh();
        await WaitAsync("transcriptState().pending && !transcriptState().elapsedTimerActive && transcriptState().liveElapsedCount === 0", "completion stops live timing even while a selection defers body convergence");
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
        reply.Message.Status = "streaming";
        reply.Message.AssistantSegments.Clear();
        reply.Message.Content = "";
        reply.Message.GenerationStartedTimestamp = null;
        reply.Refresh();
        await WaitAsync("document.querySelector('.assistant').dataset.presentationMode === 'active' && document.querySelector('.assistant .message-elapsed').hidden", "same-ID retry preparation clears the old elapsed state");
        reply.Message.GenerationStartedTimestamp = Stopwatch.GetTimestamp();
        reply.Message.Content = "RETRY_RUNNING";
        reply.Refresh();
        await WaitAsync("document.querySelector('.assistant .message-elapsed').dataset.mode === 'live'", "same-ID retry starts from its new host timestamp");
        Check(await EvalAsync<bool>("document.querySelector('.assistant .message-elapsed').textContent === '用时 0秒' && transcriptState().liveElapsedCount === 1"), "retry duration starts at zero instead of the previous attempt total");
        reply.Message.Status = "interrupted";
        reply.Refresh();
        await WaitAsync("document.querySelector('.assistant .message-elapsed').hidden && !transcriptState().elapsedTimerActive && transcriptState().liveElapsedCount === 0", "cancellation stops and removes the live interval");

        // Cover the coalesced retry path where a waiting snapshot never reaches the browser.
        // 覆盖等待快照被合并、未到达浏览器的重试路径；结束状态也必须重置下一次计时。
        reply.Message.Status = "streaming";
        reply.Message.GenerationStartedTimestamp = Stopwatch.GetTimestamp();
        reply.Refresh();
        await WaitAsync("document.querySelector('.assistant .message-elapsed').dataset.mode === 'live'", "direct interrupted-to-streaming transition begins a fresh attempt");
        Check(await EvalAsync<bool>("document.querySelector('.assistant .message-elapsed').textContent === '用时 0秒'"), "coalesced retry cannot reuse an older clock anchor");
        reply.Message.Status = "error";
        reply.Message.Error = "TIMING_TEST_FAILURE";
        reply.Refresh();
        await WaitAsync("document.querySelector('.assistant .message-status').textContent.includes('TIMING_TEST_FAILURE') && !transcriptState().elapsedTimerActive && document.querySelector('.assistant .message-elapsed').hidden", "error removes the clock and retains its visible failure state");
    }
}
