using System.Collections;
using System.Diagnostics;
using System.Reflection;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.ViewModels;

namespace TranscriptUiSmoke;

public partial class App
{
    // Keep cache/navigation regression checks independent of clipboard and foreground input.
    // 缓存与导航回归独立运行，不依赖剪贴板或前台输入。
    private async Task CheckConversationSwitchingOnlyAsync()
    {
        var chat = Guid.NewGuid();
        var rows = new List<ConversationMessageViewModel>
        {
            Message(chat, "user", "ISOLATED_NAVIGATION_FIXTURE"),
            Message(chat, "assistant", "# Cached mathematics\n\n$x^2+y^2$\n\n" +
                string.Join("\n\n", Enumerable.Repeat("缓存正文 / cached body", 60)) + "\n\nFOLLOW_APPEND")
        };
        _transcript.ShowConversation(chat, rows);
        await WaitAsync("document.querySelector('.katex') !== null && __transcriptSmoke.bottomDistance() < 4", "isolated navigation fixture is rendered");
        await CheckConversationSwitchingAsync(chat, rows);
        _metrics["checks"] = _checks;
        _metrics["mode"] = "conversation-switching";
        File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"),
            System.Text.Json.JsonSerializer.Serialize(_metrics, new System.Text.Json.JsonSerializerOptions { WriteIndented = true }));
        File.WriteAllText(_result, $"PASS: {_checks} conversation switching checks.");
    }

    private async Task CheckConversationSwitchingAsync(Guid longChat, List<ConversationMessageViewModel> longRows)
    {
        var html = (IDictionary)typeof(ConversationTranscript).GetField("_html", BindingFlags.Instance | BindingFlags.NonPublic)!.GetValue(_transcript)!;
        object? cachedHtml = html[longRows[1].Message.Id];
        Check(cachedHtml is not null, "long conversation HTML is cached");
        await EvalAsync<bool>("(() => { window.__cachedArticle = document.querySelectorAll('.message')[1]; window.__cachedMath = document.querySelector('.katex'); return true; })()");
        var shortChat = Guid.NewGuid();
        var shortRows = new List<ConversationMessageViewModel> { Message(shortChat, "assistant", "SHORT_SWITCH_FIXTURE") };
        _transcript.ShowConversation(shortChat, shortRows);
        await WaitAsync("__transcriptSmoke.bodyText().includes('SHORT_SWITCH_FIXTURE') && window.transcriptState().messageCount === 1", "switch to short conversation");
        var warm = Stopwatch.StartNew();
        _transcript.ShowConversation(longChat, longRows);
        await WaitAsync("__transcriptSmoke.bodyText().includes('FOLLOW_APPEND') && __transcriptSmoke.bottomDistance() < 4", "cached conversation opens immediately at bottom");
        _metrics["warmReopen150Ms"] = warm.ElapsedMilliseconds;
        Check(ReferenceEquals(cachedHtml, html[longRows[1].Message.Id]), "switch reuses cached Markdown HTML");
        Check(await EvalAsync<bool>("document.querySelectorAll('.message')[1] === window.__cachedArticle && document.querySelector('.katex') === window.__cachedMath"),
            "switch restores the same message and KaTeX DOM nodes");
        await EvalAsync<bool>("(() => { window.__switchMutations = 0; window.__switchMutationDetails = []; window.__switchObserver = new MutationObserver(records => { window.__switchMutations += records.length; window.__switchMutationDetails.push(...records.slice(0, 40).map(record => ({ type:record.type, tag:record.target.nodeName, classes:record.target.className, attribute:record.attributeName, added:record.addedNodes.length, removed:record.removedNodes.length }))); }); window.__switchObserver.observe(document.querySelector('#messages'), { childList:true, subtree:true, attributes:true, characterData:true }); return true; })()");
        _transcript.ShowConversation(longChat, longRows, openAtBottom: false);
        await Task.Delay(120);
        _metrics["unchangedConversationMutations"] = await EvalAsync<System.Text.Json.JsonElement>("window.__switchMutationDetails");
        Check(await EvalAsync<int>("window.__switchMutations") == 0, "unchanged conversation does not mutate message DOM");
        await EvalAsync<bool>("(() => { window.__switchObserver.disconnect(); return true; })()");

        // Force an uncached parse, then supersede it before its background task posts.
        // 强制执行未缓存的解析，并在后台任务回写之前用新请求取代。
        var abandonedChat = Guid.NewGuid();
        var abandonedRows = Enumerable.Range(0, 80).Select(index => Message(abandonedChat, "assistant",
            $"ABANDONED_CHAT_{index}\n\n```python\n" + string.Join('\n', Enumerable.Repeat("value = 1 + 2  # abandoned generation", 80)) + "\n```" )).ToList();
        _transcript.ShowConversation(abandonedChat, abandonedRows);
        _transcript.ShowConversation(shortChat, shortRows);
        await WaitAsync("__transcriptSmoke.bodyText().includes('SHORT_SWITCH_FIXTURE')", "rapid navigation restores latest chat before old parse finishes");
        await Task.Delay(700);
        Check(await EvalAsync<string>("window.transcriptState().conversationId") == shortChat.ToString() &&
            !await EvalAsync<bool>("__transcriptSmoke.bodyText().includes('ABANDONED_CHAT')"), "obsolete parse cannot overwrite active chat");

        for (int index = 0; index < 7; index++)
        {
            var chat = Guid.NewGuid();
            _transcript.ShowConversation(chat, [Message(chat, "assistant", "CACHE_LIMIT_" + index)]);
            await WaitAsync($"__transcriptSmoke.bodyText().includes('CACHE_LIMIT_{index}')", "bounded cache fixture " + index);
        }
        Check(await EvalAsync<bool>("window.transcriptState().cachedConversations <= 4 && window.transcriptState().cachedNodes <= 40000 && window.transcriptState().cachedCharacters <= 2097152"),
            "detached conversation DOM cache stays within count, node and text bounds");
    }
}
