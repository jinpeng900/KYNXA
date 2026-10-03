using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;

namespace TranscriptUiSmoke;

public partial class App
{
    private async Task CheckLiveLanguageAsync()
    {
        string originalLanguage = UiText.Language;
        try
        {
            UiText.Initialize("zh-CN");
            var cachedChat = Guid.NewGuid();
            var cached = Message(cachedChat, "assistant", "CACHED_LANGUAGE_BODY 缓存原文 $x=1$", "interrupted");
            cached.Message.Reasoning = "CACHED_REASONING 原始思考内容";
            cached.Message.ReasoningDurationMs = 3200;
            cached.Message.Error = "CACHED_PROVIDER_ERROR 原始错误";
            cached.SetRetryAllowed(true);
            _transcript.ShowConversation(cachedChat, [cached]);
            await WaitAsync("!!(document.querySelector('.message-body')?.textContent.includes('CACHED_LANGUAGE_BODY') && document.querySelector('.reasoning summary')?.textContent === '思考过程 · 4 秒')", "cached language fixture renders in Chinese");
            await EvalAsync<bool>("(() => { window.__languageCached = document.querySelector('#messages > article'); return true; })()");

            var chat = Guid.NewGuid();
            var stopped = Message(chat, "assistant",
                "LANGUAGE_BODY 开始 $\\omega=2\\pi f$。\n\n```python\nprint('正在生成 stays literal')\n```\n\n"
                + string.Join("\n\n", Enumerable.Range(0, 25).Select(index => $"原始段落 {index}: Copy 重试 思考过程 must stay unchanged."))
                + "\n\nLANGUAGE_END 结束", "interrupted");
            stopped.Message.Reasoning = "REASONING_BODY 原始思考，不随界面翻译。";
            stopped.Message.ReasoningDurationMs = 2200;
            stopped.Message.Error = "PROVIDER_ERROR 服务端原始错误";
            stopped.SetRetryAllowed(true);
            var thinking = Message(chat, "assistant", "", "streaming");
            thinking.Message.Reasoning = "ACTIVE_REASONING 仍然是原文。";
            thinking.Refresh(isThinking: true);
            var rows = new List<ConversationMessageViewModel>
            {
                Message(chat, "user", "LANGUAGE_USER 用户原文：复制，思考过程，重试。"),
                stopped, thinking, Message(chat, "assistant", "", "streaming")
            };
            string originalContent = stopped.Message.Content, originalReasoning = stopped.Message.Reasoning;
            _transcript.ShowConversation(chat, rows);
            await WaitAsync("!!(document.querySelectorAll('#messages > article').length === 4 && document.querySelectorAll('.katex').length === 1 && [...document.querySelectorAll('.reasoning summary')].some(node => node.textContent === '正在思考…'))", "live language fixture has body, math, stopped, thinking and waiting states");
            await EvalAsync<bool>("__transcriptSmoke.openReasoning(true)");
            await EvalAsync<bool>("__transcriptSmoke.scrollUp()");
            await Task.Delay(150);
            await EvalAsync<bool>("""
                (() => {
                  const first = __transcriptSmoke.textNode(document.querySelector('[data-role=user] .message-body'), 'LANGUAGE_USER');
                  const last = __transcriptSmoke.textNode(document.getElementById('messages'), 'LANGUAGE_END');
                  const selection = getSelection();
                  selection.setBaseAndExtent(first, 2, last, last.nodeValue.length - 1);
                  const roots = [...document.querySelectorAll('.message-body, .reasoning-body')];
                  const nodes = roots.flatMap(root => [root, ...root.querySelectorAll('*')]);
                  const probe = window.__languageProbe = {
                    document, roots, nodes, html: roots.map(root => root.innerHTML),
                    anchorNode: selection.anchorNode, anchorOffset: selection.anchorOffset,
                    focusNode: selection.focusNode, focusOffset: selection.focusOffset,
                    copy: window.transcriptSelectionText(), top: scrollY, left: scrollX,
                    mutations: 0, renders: 0
                  };
                  probe.observer = new MutationObserver(records => { probe.mutations += records.length; });
                  roots.forEach(root => probe.observer.observe(root, {childList:true, characterData:true, attributes:true, subtree:true}));
                  probe.listener = event => { if (event.data?.type === 'render') probe.renders++; };
                  window.chrome.webview.addEventListener('message', probe.listener);
                  return true;
                })()
                """);

            UiText.Initialize("en");
            await WaitAsync("!!(document.documentElement.lang === 'en' && [...document.querySelectorAll('.copy-message')].every(button => button.title === 'Copy'))", "existing transcript controls change to English without navigation");
            await Task.Delay(100);
            Check(await EvalAsync<bool>("document.title === 'Conversation' && document.getElementById('messages').getAttribute('aria-label') === 'Chat history'"), "document title and transcript accessibility label change immediately");
            Check(await EvalAsync<bool>("[...document.querySelectorAll('.copy-message')].every(button => button.getAttribute('aria-label') === 'Copy entire message') && [...document.querySelectorAll('.retry')].every(button => button.textContent === 'Retry')"), "copy and retry controls change on existing messages");
            Check(await EvalAsync<bool>("[...document.querySelectorAll('.reasoning summary')].some(node => node.textContent === 'Reasoning · 3 s') && [...document.querySelectorAll('.reasoning summary')].some(node => node.textContent === 'Thinking…')"), "finished and active reasoning titles use the new language");
            Check(await EvalAsync<bool>("[...document.querySelectorAll('.message-status')].some(node => node.textContent === 'Generation stopped · PROVIDER_ERROR 服务端原始错误') && [...document.querySelectorAll('.message-status')].some(node => node.textContent === 'Replying…') && [...document.querySelectorAll('.streaming-dot')].every(node => node.getAttribute('aria-label') === 'Generating')"), "status chrome changes while provider error text is preserved");
            Check(stopped.ReasoningTitle == "Reasoning · 3 s" && stopped.ErrorText == "Generation stopped · PROVIDER_ERROR 服务端原始错误", "view model UI getters use the current language");
            Check(stopped.Message.Content == originalContent && stopped.Message.Reasoning == originalReasoning, "language switching does not modify model content");
            Check(await EvalAsync<bool>("window.__languageProbe.renders === 0 && window.__languageProbe.document === document"), "language switch sends no transcript render and keeps the document");
            await CheckLanguageBodyPreservationAsync();
            Check(await EvalAsync<bool>("!window.__languageCached.isConnected && window.__languageCached.querySelector('.reasoning summary').textContent === 'Reasoning · 4 s' && window.__languageCached.querySelector('.message-status').textContent === 'Generation stopped · CACHED_PROVIDER_ERROR 原始错误' && window.__languageCached.querySelector('.retry').textContent === 'Retry'"), "detached cached conversation controls and reasoning titles also switch");

            // A queued stream update must remain deferred while UI labels switch.
            thinking.Message.Reasoning += " PENDING_LANGUAGE_UPDATE";
            thinking.Refresh(isThinking: true);
            await WaitAsync("!!(window.transcriptState().pending)", "selected transcript queues a model update before the next language switch");
            int rendersBefore = await EvalAsync<int>("window.__languageProbe.renders");
            UiText.Initialize("zh-CN");
            await WaitAsync("!!(document.documentElement.lang === 'zh-CN' && [...document.querySelectorAll('.reasoning summary')].some(node => node.textContent === '思考过程 · 3 秒'))", "existing titles switch back to Chinese immediately");
            await Task.Delay(100);
            Check(await EvalAsync<int>("window.__languageProbe.renders") == rendersBefore, "switching back does not enqueue another transcript render");
            Check(await EvalAsync<bool>("window.transcriptState().pending && !document.getElementById('messages').textContent.includes('PENDING_LANGUAGE_UPDATE')"), "language switching preserves the deferred content update");
            await CheckLanguageBodyPreservationAsync();
            Check(await EvalAsync<bool>("[...document.querySelectorAll('.retry')].every(node => node.textContent === '重试') && window.__languageCached.querySelector('.reasoning summary').textContent === '思考过程 · 4 秒'"), "active and cached UI return to Chinese");

            await StopLanguageProbeAsync();
            _transcript.ClearSelection();
            await WaitAsync("!!(!window.transcriptState().pending && document.getElementById('messages').textContent.includes('PENDING_LANGUAGE_UPDATE'))", "clearing selection applies the pending model content after localization");
            Check(await EvalAsync<bool>("[...document.querySelectorAll('.reasoning summary')].some(node => node.textContent === '正在思考…')"), "deferred snapshots format UI titles in the current language");
            _transcript.ShowConversation(cachedChat, [cached]);
            await WaitAsync("!!(document.querySelector('.message-body')?.textContent.includes('CACHED_LANGUAGE_BODY'))", "cached language fixture reopens");
            Check(await EvalAsync<bool>("window.__languageCached === document.querySelector('#messages > article') && window.__languageCached.querySelector('.reasoning summary').textContent === '思考过程 · 4 秒'"), "cached conversation reuses its DOM with translated metadata");
            _metrics["liveLanguageSwitching"] = "passed: active/cached labels, no body mutations or render commands, selection/scroll, deferred updates";
        }
        finally
        {
            await StopLanguageProbeAsync();
            _transcript.ClearSelection();
            UiText.Initialize(originalLanguage);
        }
    }

    private async Task CheckLanguageBodyPreservationAsync()
    {
        Check(await EvalAsync<bool>("window.__languageProbe.mutations === 0 && window.__languageProbe.roots.every((root, index) => root.innerHTML === window.__languageProbe.html[index]) && window.__languageProbe.nodes.every(node => node.isConnected)"), "language change preserves message, code, reasoning and formula DOM");
        Check(await EvalAsync<bool>("(() => { const saved = window.__languageProbe, selection = getSelection(); return selection.anchorNode === saved.anchorNode && selection.anchorOffset === saved.anchorOffset && selection.focusNode === saved.focusNode && selection.focusOffset === saved.focusOffset && window.transcriptSelectionText() === saved.copy; })()"), "language change preserves cross-message selection endpoints and exact copied text");
        Check(await EvalAsync<bool>("Math.abs(scrollY - window.__languageProbe.top) < 2 && Math.abs(scrollX - window.__languageProbe.left) < 2"), "language change preserves the manual scroll position");
        Check(await EvalAsync<bool>("document.querySelector('.reasoning:not([hidden])').open"), "language change preserves expanded reasoning");
    }

    private Task<bool> StopLanguageProbeAsync() => EvalAsync<bool>("(() => { const saved = window.__languageProbe; if (saved) { saved.observer.disconnect(); window.chrome.webview.removeEventListener('message', saved.listener); delete window.__languageProbe; } return true; })()");
}
