using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using Windows.ApplicationModel.DataTransfer;

namespace TranscriptUiSmoke;

public partial class App
{
    private void CheckPresentationSources()
    {
        var segments = Enumerable.Range(1, 6).Select(round => new AssistantSegment("source-" + round, round, round * 3,
            "commentary", round == 6 ? "streaming" : "completed", "BODY_" + round, "PUBLIC_THOUGHT_" + round)).ToList();
        var tools = Enumerable.Range(0, 210).Select(index => new ToolActivity("source-tool-" + index, "filesystem.read",
            JsonSerializer.SerializeToElement(new { path = "sample-" + index + ".txt" }), "completed", "Read source", Round: 5, Order: 16)).ToList();
        tools.Insert(0, new("approval-old", "filesystem.write", JsonSerializer.SerializeToElement(new { path = "pending.txt" }),
            "approval-required", "Pending source", Round: 1, Order: 4));
        string before = JsonSerializer.Serialize(new { segments, tools });
        var active = TranscriptPresentation.Select("assistant", "streaming", "legacy body", segments, tools);
        Check(active.Mode == "active" && active.Segments.Select(segment => segment.Round).SequenceEqual([1, 4, 5, 6]), "C# presentation keeps recent three stages and old pending approval stage");
        Check(active.Tools.Length == 9 && active.Tools[0].ToolCallId == "approval-old" && active.Tools[^1].ToolCallId == "source-tool-209", "C# presentation caps ordinary actions at eight and preserves approval");
        Check(active.Content.Contains("BODY_1") && active.Content.Contains("BODY_6") && !active.Content.Contains("BODY_2"), "active copy projection equals visible stage text");
        segments[^1] = segments[^1] with { Phase = "final_answer", Status = "completed", Content = "FINAL_EXACT" };
        tools[0] = tools[0] with { Status = "completed" };
        var final = TranscriptPresentation.Select("assistant", "completed", "legacy accumulated stages", segments, tools);
        Check(final.Mode == "final" && final.Content == "FINAL_EXACT" && final.Segments.Length == 1 && final.Tools.Length == 0, "C# final projection contains only actual completed final text");
        var incomplete = TranscriptPresentation.Select("assistant", "completed", "unverified fallback", segments.Take(5).ToArray(), tools);
        Check(incomplete.Mode == "incomplete" && incomplete.Content == "BODY_5", "completed status without final phase does not become a successful final answer");
        Check(TranscriptPresentation.Select("assistant", "completed", " \n ", [], []).Mode == "incomplete", "empty legacy completed content cannot invent a finished answer");
        Check(TranscriptPresentation.Select("assistant", "completed", "legacy completed text", [], []).Mode == "final", "valid legacy final content remains compatible");
        Check(TranscriptPresentation.Select("assistant", "interrupted", "legacy partial", [], []).Content == "legacy partial", "legacy interrupted body remains visible");
        var partial = TranscriptPresentation.Select("assistant", "interrupted", "old aggregate", segments, tools);
        Check(partial.Mode == "partial" && partial.Content == "FINAL_EXACT" && partial.Segments.Length == 1, "interrupted status keeps the latest partial body despite a completed-looking segment");
        // Selection creates arrays; it never removes the authoritative source collection.
        Check(segments.Count == 6 && tools.Count == 211 && before.Contains("BODY_2"), "all source stages and actions survive bounded projection");
        string after = JsonSerializer.Serialize(new { segments, tools });
        _ = TranscriptPresentation.Select("assistant", "streaming", "legacy body", segments, tools);
        Check(after == JsonSerializer.Serialize(new { segments, tools }), "C# Select never mutates source events");
    }

    private async Task CheckAssistantTimelineAsync()
    {
        var chat = Guid.NewGuid();
        var user = Message(chat, "user", "TIMELINE_USER 请求核验来源。");
        var reply = Message(chat, "assistant", "FINAL_DRAFT", "streaming");
        reply.Message.AssistantSegments =
        [
            new("round-1", 1, 0, "commentary", "completed", "STAGE_ONE\n\nChecked the first source.", "PUBLIC_SUMMARY", 1000),
            new("round-2", 2, 2, "commentary", "completed", "STAGE_TWO\n\nChecked the second source.", "SECOND_PUBLIC_SUMMARY", 2000),
            new("round-3", 3, 4, "commentary", "streaming", "FINAL_DRAFT", "")
        ];
        reply.Message.ToolActivities =
        [
            new("timeline-1", "filesystem.read", JsonSerializer.SerializeToElement(new { path = "first.txt" }), "completed", "Read first source", "FIRST_RECEIPT", Round: 1, Order: 1),
            new("timeline-2", "filesystem.read", JsonSerializer.SerializeToElement(new { path = "second.txt" }), "completed", "Read second source", "SECOND_RECEIPT", Round: 2, Order: 3)
        ];
        _transcript.ShowConversation(chat, [user, reply]);
        await WaitAsync("document.querySelectorAll('.assistant-segment').length === 3 && document.querySelector('[data-segment-id=round-3] .message-body').textContent === 'FINAL_DRAFT'", "real DTO shows recent true prose/tool stages");
        Check(await EvalAsync<bool>("[...document.querySelectorAll('.assistant-segment')].map(row=>row.dataset.round).join(',') === '1,2,3' && !document.querySelector('.assistant-segment .reasoning')"), "active stages remain in real order without repeated thinking cards");
        Check(await EvalAsync<bool>("document.querySelector('[data-segment-id=round-1] [data-tool-call-id=timeline-1]') !== null && document.querySelector('[data-segment-id=round-2] [data-tool-call-id=timeline-2]') !== null && !!(document.querySelector('[data-segment-id=round-1] .message-body').compareDocumentPosition(document.querySelector('[data-tool-call-id=timeline-1]')) & Node.DOCUMENT_POSITION_FOLLOWING)"), "compact tool events still follow their actual stage body");
        await EvalAsync<bool>("(() => { window.__timelineFirst=document.querySelector('[data-segment-id=round-1] .message-body').firstChild; const range=document.createRange(); range.selectNodeContents(window.__timelineFirst); getSelection().removeAllRanges(); getSelection().addRange(range); return true; })()");
        reply.Message.AssistantSegments[^1] = reply.Message.AssistantSegments[^1] with { Status = "completed", Content = "STAGE_THREE" };
        reply.Message.AssistantSegments.Add(new("round-4", 4, 6, "commentary", "completed", "STAGE_FOUR", ""));
        reply.Message.AssistantSegments.Add(new("round-5", 5, 8, "commentary", "streaming", "FINAL_DRAFT", ""));
        reply.Message.ToolActivities.Add(new("timeline-3", "filesystem.read", JsonSerializer.SerializeToElement(new { path = "third.txt" }), "completed", "Third source", Round: 3, Order: 5));
        reply.Message.ToolActivities.Add(new("timeline-4", "filesystem.read", JsonSerializer.SerializeToElement(new { path = "fourth.txt" }), "completed", "Fourth source", Round: 4, Order: 7));
        reply.Refresh();
        await WaitAsync("transcriptState().pending", "stage-window eviction waits for an active text selection");
        Check(await EvalAsync<bool>("getSelection().toString() === 'STAGE_ONE' && document.querySelector('[data-segment-id=round-1] .message-body').firstChild === window.__timelineFirst"), "bounded progress does not remove currently selected text");
        _transcript.ClearSelection();
        await WaitAsync("[...document.querySelectorAll('.assistant-segment')].map(row=>row.dataset.round).join(',') === '3,4,5'", "clearing selection applies exactly the latest three actual stages");
        _metrics["compactActivePreview"] = Path.Combine(Path.GetTempPath(), "kynxa-transcript-compact-active.png");
        await CaptureViewportAsync((string)_metrics["compactActivePreview"]);
        await EvalAsync<bool>("(() => { const range=document.createRange(); range.selectNodeContents(document.querySelector('[data-segment-id=round-4] .message-body').firstChild); getSelection().removeAllRanges(); getSelection().addRange(range); return true; })()");
        reply.Message.AssistantSegments[^1] = reply.Message.AssistantSegments[^1] with
        { Phase = "final_answer", Status = "completed", Content = "FINAL_VERIFIED\n\n$\\omega=2\\pi f$" };
        reply.Message.Content = reply.Message.AssistantSegments[^1].Content;
        reply.Message.Status = "completed";
        reply.Message.DurationMs = 17000;
        reply.Refresh();
        await WaitAsync("transcriptState().pending", "final convergence waits while a stage body is selected");
        Check(await EvalAsync<bool>("getSelection().toString() === 'STAGE_FOUR' && document.querySelectorAll('.assistant-segment').length === 3"), "selected progress survives the terminal final event");
        _transcript.ClearSelection();
        await WaitAsync("document.querySelectorAll('.assistant-segment').length === 1 && document.querySelector('[data-segment-id=round-5] .katex') !== null && document.querySelector('.assistant .message-elapsed').textContent.includes('17')", "success converges to complete rendered final body and actual duration");
        Check(await EvalAsync<bool>("!document.querySelector('.assistant .reasoning,.assistant .tool-activities,.assistant .tool-activity') && !!(document.querySelector('.assistant .message-elapsed').compareDocumentPosition(document.querySelector('.final-answer')) & Node.DOCUMENT_POSITION_FOLLOWING)"), "terminal success removes process DOM and puts elapsed text above final answer");
        Check(reply.Message.AssistantSegments.Count == 5 && reply.Message.ToolActivities.Count == 4 && reply.Message.AssistantSegments[0].Content.Contains("STAGE_ONE"), "successful UI convergence preserves every formal stage and tool");
        await EvalAsync<bool>("(() => { document.querySelector('.assistant .message-actions > .copy-message').click(); return true; })()");
        await Task.Delay(100);
        Check(await Clipboard.GetContent().GetTextAsync() == reply.Message.Content, "whole-reply clipboard contains exactly the displayed final Markdown");
        await EvalAsync<bool>("(() => { window.__finalNode=document.querySelector('.final-answer .message-body').firstChild; window.__elapsed=document.querySelector('.assistant .message-elapsed').textContent; return true; })()");
        string language = UiText.Language;
        try
        {
            UiText.Initialize("en");
            await WaitAsync("document.querySelector('.assistant .message-elapsed').textContent !== window.__elapsed && document.querySelector('.assistant .message-elapsed').textContent.includes('17')", "elapsed duration changes interface language immediately");
            Check(await EvalAsync<bool>("document.querySelector('.final-answer .message-body').firstChild === window.__finalNode && !/[\\u3400-\\u9fff]/u.test(document.querySelector('.assistant .message-elapsed').textContent)"), "duration localization preserves final prose and formula DOM");
        }
        finally { UiText.Initialize(language); }
        _metrics["assistantTimelinePreview"] = Path.Combine(Path.GetTempPath(), "kynxa-transcript-timeline.png");
        await CaptureViewportAsync((string)_metrics["assistantTimelinePreview"]);
        _transcript.ShowConversation(Guid.NewGuid(), []);
        _transcript.ShowConversation(chat, [user, reply]);
        await WaitAsync("document.querySelectorAll('.assistant-segment').length === 1 && document.querySelector('[data-segment-id=round-5]').dataset.phase === 'final_answer'", "reopening saved completed history stays converged to final text");
        Check(await EvalAsync<bool>("document.querySelector('.final-answer .message-body').firstChild === window.__finalNode && !document.querySelector('.tool-activity')"), "cached reopen reuses final body without resurrecting process");
    }

    private async Task CheckPartialProgressAsync()
    {
        foreach (string status in new[] { "interrupted", "error", "completed" })
        {
            var chat = Guid.NewGuid();
            var reply = Message(chat, "assistant", "OLD_AGGREGATE", status);
            string body = "PARTIAL_" + status + "\n\n```python\n    print('unfinished task')\n```";
            reply.Message.AssistantSegments =
            [
                new("partial-old", 1, 0, "commentary", "completed", "EARLIER_PROCESS_MUST_DISAPPEAR", "PUBLIC_OLD_THOUGHT"),
                new("partial-last", 2, 2, "commentary", "interrupted", body, "")
            ];
            reply.Message.Error = status == "completed" ? "" : "模拟执行中断，请重试。";
            reply.SetRetryAllowed(true);
            _transcript.ShowConversation(chat, [reply]);
            await WaitAsync("!!document.querySelector('.assistant-segment .message-body') && document.querySelector('.assistant-segment .message-body').textContent.includes('PARTIAL_" + status + "') && document.querySelectorAll('.assistant-segment').length === 1", "failed or incomplete request keeps latest readable body rather than old process");
            Check(await EvalAsync<bool>("!document.getElementById('messages').textContent.includes('EARLIER_PROCESS_MUST_DISAPPEAR') && !document.querySelector('.final-answer') && !document.querySelector('.reasoning,.tool-activity')"), "partial state does not claim completed final or resurrect discarded process");
            Check(await EvalAsync<bool>("document.querySelector('.message-status').textContent.trim().length > 0"), "interruption or missing final phase has a concise visible reason");
            await EvalAsync<bool>("(() => { document.querySelector('.assistant .copy-message').click(); return true; })()");
            await Task.Delay(80);
            Check(await Clipboard.GetContent().GetTextAsync() == body, "partial copy matches the visible source Markdown exactly");
            Check(reply.Message.AssistantSegments.Count == 2 && reply.Message.Content == "OLD_AGGREGATE", "partial presentation preserves all underlying source text");
        }
    }

    private async Task CheckProgressScrollAnchorAsync()
    {
        var chat = Guid.NewGuid();
        var history = Message(chat, "assistant", "BEFORE_PROGRESS\n\n" + string.Join("\n\n", Enumerable.Range(0, 24).Select(index => "Earlier public paragraph " + index)));
        var reply = Message(chat, "assistant", "CURRENT_DRAFT", "streaming");
        reply.Message.AssistantSegments = Enumerable.Range(1, 3).Select(round => new AssistantSegment("anchor-round-" + round,
            round, round * 2, "commentary", round == 3 ? "streaming" : "completed",
            "ACTIVE_STAGE_" + round + "\n\n" + string.Join("\n\n", Enumerable.Range(0, 35).Select(index => "Work progress paragraph " + index)), "")).ToList();
        var later = Message(chat, "assistant", "ANCHOR_AFTER_PROGRESS\n\n" + string.Join("\n\n", Enumerable.Range(0, 80).Select(index => "Following public paragraph " + index)));
        _transcript.ShowConversation(chat, [history, reply, later]);
        await WaitAsync("document.querySelectorAll('.assistant-segment').length === 3 && document.documentElement.scrollHeight > innerHeight * 3", "long recent progress provides a real scroll-collapse surface");
        await EvalAsync<bool>("(() => { const rows=[...document.querySelectorAll('#messages > article')]; window.__collapseAnchor=rows[2].querySelector('.message-body p'); window.__collapseAnchor.scrollIntoView({block:'start'}); scrollBy(0,-80); return true; })()");
        await Task.Delay(100);
        await EvalAsync<bool>("(() => { window.__collapseAnchorTop=window.__collapseAnchor.getBoundingClientRect().top; return true; })()");
        reply.Message.AssistantSegments[^1] = reply.Message.AssistantSegments[^1] with { Phase = "final_answer", Status = "completed", Content = "SHORT_FINAL_VERIFIED" };
        reply.Message.Content = "SHORT_FINAL_VERIFIED";
        reply.Message.Status = "completed";
        reply.Message.DurationMs = 42000;
        reply.Refresh();
        await WaitAsync("document.querySelectorAll('.assistant-segment').length === 1 && document.querySelector('.final-answer').textContent.includes('SHORT_FINAL_VERIFIED')", "successful collapse removes earlier tall progress blocks");
        await Task.Delay(100);
        Check(await EvalAsync<bool>("window.__collapseAnchor.isConnected && Math.abs(window.__collapseAnchor.getBoundingClientRect().top-window.__collapseAnchorTop) < 3 && __transcriptSmoke.bottomDistance() > 100"), "user reading later text keeps its visible paragraph anchor while earlier progress shrinks");
        await EvalAsync<bool>("(() => { window.__collapseEarlier=document.querySelector('#messages > article .message-body p'); window.__collapseEarlier.scrollIntoView({block:'start'}); return true; })()");
        await Task.Delay(80);
        Check(await EvalAsync<bool>("scrollY < 150 && __transcriptSmoke.bottomDistance() > 100"), "reading earlier history after convergence stays away from automatic bottom follow");
        _metrics["compactFinalPreview"] = Path.Combine(Path.GetTempPath(), "kynxa-transcript-compact-final.png");
        await EvalAsync<bool>("(() => { document.querySelector('.final-answer').scrollIntoView({block:'center'}); return true; })()");
        await CaptureViewportAsync((string)_metrics["compactFinalPreview"]);
    }
}
