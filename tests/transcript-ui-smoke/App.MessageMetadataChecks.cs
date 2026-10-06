using System.Text.Json;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Windows.ApplicationModel.DataTransfer;

namespace TranscriptUiSmoke;

public partial class App
{
    private async Task CheckMessageMetadataAsync()
    {
        string language = UiText.Language;
        var created = new DateTimeOffset(2026, 4, 18, 2, 3, 4, TimeSpan.Zero);
        var observedEnd = created.AddHours(3).AddSeconds(5);
        const string copiedMarkdown = "METADATA_COPY_BODY 原文 😀\n\n```cs\n    int value = 1;\n```\n\n$\\alpha$";
        var cachedChat = Guid.NewGuid();
        var cached = Message(cachedChat, "assistant", "METADATA_CACHED_BODY 原始缓存 $x^2$");
        cached.Message.CreatedAt = created.AddDays(-1);
        cached.Message.DurationMs = 1200;
        cached.Message.Reasoning = "PRIVATE_METADATA_REASONING";
        string cachedOriginal = JsonSerializer.Serialize(cached.Message);
        try
        {
            UiText.Initialize("zh-CN");
            _transcript.ClearSelection();
            MessageTimePresentation.RecordEnd(cached.Message, observedEnd.AddDays(-1));
            MessageTimePresentation.RecordEnd(cached.Message, observedEnd.AddDays(1));
            Check(MessageTimePresentation.GetEnd(cached.Message) == observedEnd.AddDays(-1) &&
                JsonSerializer.Serialize(cached.Message) == cachedOriginal,
                "first local terminal observation is stable and changes no formally serialized message fields or original text");
            var sameIdHistory = JsonSerializer.Deserialize<ChatMessageState>(cachedOriginal)!;
            Check(sameIdHistory.Id == cached.Message.Id && MessageTimePresentation.GetEnd(sameIdHistory) is null,
                "a reloaded message with the same ID does not inherit another object's transient end observation");
            _transcript.ShowConversation(cachedChat, [cached]);
            await WaitAsync("__transcriptSmoke.bodyText().includes('METADATA_CACHED_BODY') && !!document.querySelector('.katex')",
                "the metadata fixture first caches a real rendered conversation");
            await EvalAsync<bool>("""
                (() => {
                  window.__metadataCachedArticle = document.querySelector('#messages > article');
                  window.__metadataCachedBody = __metadataCachedArticle.querySelector('.message-body').innerHTML;
                  window.__metadataSmoke = {
                    article: id => document.querySelector(`article[data-message-id="${id}"]`),
                    localTime: value => {
                      const d = new Date(value), pad = number => String(number).padStart(2, '0');
                      return `${d.getFullYear()}/${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
                    }
                  };
                  return true;
                })()
                """);

            var chat = Guid.NewGuid();
            var user = Message(chat, "user", "USER_METADATA_LITERAL 用户原文\n  保留缩进。");
            user.Message.CreatedAt = created.AddMinutes(-1);
            var completed = Message(chat, "assistant", copiedMarkdown, "streaming");
            completed.Message.CreatedAt = created;
            var history = Message(chat, "assistant", "METADATA_HISTORY_KNOWN");
            history.Message.CreatedAt = created.AddDays(-2);
            history.Message.DurationMs = 9876;
            var unknown = Message(chat, "assistant", "METADATA_HISTORY_UNKNOWN");
            unknown.Message.CreatedAt = DateTimeOffset.UnixEpoch;
            unknown.Message.DurationMs = 4444;
            var cancelled = Message(chat, "assistant", "METADATA_CANCELLED_BODY", "streaming");
            cancelled.Message.CreatedAt = created.AddMinutes(1);
            var failed = Message(chat, "assistant", "METADATA_FAILED_BODY", "streaming");
            failed.Message.CreatedAt = created.AddMinutes(2);
            MessageTimePresentation.RecordEnd(user.Message, observedEnd);
            MessageTimePresentation.RecordEnd(completed.Message, observedEnd);
            Check(MessageTimePresentation.GetEnd(user.Message) is null && MessageTimePresentation.GetEnd(completed.Message) is null &&
                MessageTimePresentation.GetEnd(history.Message) is null,
                "user, active generation and unobserved historical completion do not fabricate an end time");
            var rows = new List<ConversationMessageViewModel> { user, completed, history, unknown, cancelled, failed };
            _transcript.ShowConversation(chat, rows);
            await WaitAsync("document.querySelectorAll('#messages > article').length === 6 && __transcriptSmoke.bodyText().includes('METADATA_FAILED_BODY')",
                "synthetic user, streaming and historical message states render in the production transcript");
            Check(await EvalAsync<bool>($$"""
                (() => {
                  const s = __metadataSmoke, u = s.article('{{user.Message.Id}}'), active = s.article('{{completed.Message.Id}}');
                  const historic = s.article('{{history.Message.Id}}'), unknown = s.article('{{unknown.Message.Id}}');
                  return u.title === '发送时间：' + s.localTime({{user.Message.CreatedAt.ToUnixTimeMilliseconds()}})
                    && active.title === '回复创建时间：' + s.localTime({{created.ToUnixTimeMilliseconds()}}) + '\n回复结束时间：正在生成'
                    && historic.title.includes('回复结束时间：未记录') && !historic.title.includes('本机记录')
                    && unknown.title.startsWith('回复创建时间：未记录\n回复结束时间：未记录')
                    && [...document.querySelectorAll('#messages > article')].every(article => article.title === article.getAttribute('aria-description'));
                })()
                """), "message title and accessible description distinguish sending, active generation and unknown historical times in local time");

            completed.Message.Status = "completed";
            completed.Message.DurationMs = 1456;
            string completedOriginal = JsonSerializer.Serialize(completed.Message);
            MessageTimePresentation.RecordEnd(completed.Message, observedEnd);
            MessageTimePresentation.RecordEnd(completed.Message, observedEnd.AddHours(1));
            completed.Refresh();
            cancelled.Message.Status = "interrupted";
            MessageTimePresentation.RecordEnd(cancelled.Message, observedEnd.AddSeconds(1));
            cancelled.Refresh();
            failed.Message.Status = "error";
            failed.Message.Error = "Synthetic failure without a model call.";
            MessageTimePresentation.RecordEnd(failed.Message, observedEnd.AddSeconds(2));
            failed.Refresh();
            await WaitAsync($$"""
                __metadataSmoke.article('{{completed.Message.Id}}').title.includes('回复结束时间（本机记录）：' + __metadataSmoke.localTime({{observedEnd.ToUnixTimeMilliseconds()}}))
                && __metadataSmoke.article('{{cancelled.Message.Id}}').title.includes('回复结束时间（本机记录）：' + __metadataSmoke.localTime({{observedEnd.AddSeconds(1).ToUnixTimeMilliseconds()}}))
                && __metadataSmoke.article('{{failed.Message.Id}}').title.includes('回复结束时间（本机记录）：' + __metadataSmoke.localTime({{observedEnd.AddSeconds(2).ToUnixTimeMilliseconds()}}))
                """, "completed, cancelled and failed synthetic transitions display their actual supplied local observations");
            Check(MessageTimePresentation.GetEnd(completed.Message) == observedEnd && JsonSerializer.Serialize(completed.Message) == completedOriginal &&
                await EvalAsync<bool>($$"""
                    !__metadataSmoke.article('{{completed.Message.Id}}').title.includes(__metadataSmoke.localTime({{created.AddMilliseconds(completed.Message.DurationMs).ToUnixTimeMilliseconds()}}))
                    && __metadataSmoke.article('{{history.Message.Id}}').title.includes('回复结束时间：未记录')
                    """), "end display uses the first observation rather than CreatedAt plus DurationMs and preserves source serialization");

            await EvalAsync<bool>($$"""
                (() => {
                  window.__metadataHistoryBody = __metadataSmoke.article('{{history.Message.Id}}').querySelector('.message-body');
                  window.__metadataHistoryHtml = __metadataHistoryBody.innerHTML;
                  return true;
                })()
                """);
            history.Message.CreatedAt = history.Message.CreatedAt.AddDays(1);
            history.Refresh();
            await WaitAsync($$"""
                __metadataSmoke.article('{{history.Message.Id}}').title.startsWith('回复创建时间：' + __metadataSmoke.localTime({{history.Message.CreatedAt.ToUnixTimeMilliseconds()}}))
                """, "a timestamp-only snapshot change refreshes the hover metadata");
            Check(await EvalAsync<bool>($$"""
                __metadataHistoryBody === __metadataSmoke.article('{{history.Message.Id}}').querySelector('.message-body')
                && __metadataHistoryBody.innerHTML === __metadataHistoryHtml
                """), "updating a timestamp does not replace or change the original message body DOM");

            string selected = await EvalAsync<string>("""
                (() => {
                  const messages = document.getElementById('messages');
                  const first = __transcriptSmoke.textNode(messages, 'USER_METADATA_LITERAL');
                  const last = __transcriptSmoke.textNode(messages, 'METADATA_FAILED_BODY');
                  const selection = getSelection();
                  selection.removeAllRanges(); selection.setBaseAndExtent(first, 0, last, last.nodeValue.length);
                  return __transcriptSmoke.copyEvent();
                })()
                """);
            Check(selected.Contains("USER_METADATA_LITERAL") && selected.Contains("METADATA_COPY_BODY") && selected.Contains(@"\alpha") &&
                !selected.Contains("发送时间") && !selected.Contains("回复结束时间") && !selected.Contains("本机记录") && !selected.Contains("未记录"),
                "cross-message selection keeps original message and TeX content and excludes all hover timestamps");
            await EvalAsync<bool>("""
                (() => {
                  const selection = getSelection();
                  window.__metadataSelection = { text: window.transcriptSelectionText(), anchor: selection.anchorNode,
                    anchorOffset: selection.anchorOffset, focus: selection.focusNode, focusOffset: selection.focusOffset };
                  window.__metadataBodyHtml = [...document.querySelectorAll('.message-body')].map(body => body.innerHTML);
                  return true;
                })()
                """);
            UiText.Initialize("en");
            string sentLabel = JsonSerializer.Serialize(UiText.Get("发送时间"));
            string createdLabel = JsonSerializer.Serialize(UiText.Get("回复创建时间"));
            string endedLabel = JsonSerializer.Serialize(UiText.Get("回复结束时间"));
            string localLabel = JsonSerializer.Serialize(UiText.Get("本机记录"));
            await WaitAsync($$"""
                document.documentElement.lang === 'en'
                && __metadataSmoke.article('{{user.Message.Id}}').title.startsWith({{sentLabel}} + ': ')
                && __metadataSmoke.article('{{completed.Message.Id}}').title.includes({{endedLabel}} + ' (' + {{localLabel}} + '): ')
                && __metadataCachedArticle.title.startsWith({{createdLabel}} + ': ')
                """, "active and detached cached message times switch to English without reopening the conversation");
            Check(await EvalAsync<bool>("""
                (() => {
                  const selection = getSelection(), saved = __metadataSelection;
                  return !__metadataCachedArticle.isConnected
                    && __metadataCachedArticle.querySelector('.message-body').innerHTML === __metadataCachedBody
                    && [...document.querySelectorAll('.message-body')].every((body, i) => body.innerHTML === __metadataBodyHtml[i])
                    && selection.anchorNode === saved.anchor && selection.anchorOffset === saved.anchorOffset
                    && selection.focusNode === saved.focus && selection.focusOffset === saved.focusOffset
                    && window.transcriptSelectionText() === saved.text
                    && [...document.querySelectorAll('#messages > article')].every(article => article.title === article.getAttribute('aria-description'));
                })()
                """), "metadata localization preserves body DOM, exact selection and current/cached accessibility descriptions");
            _transcript.ClearSelection();
            _transcript.ShowConversation(cachedChat, [cached]);
            await WaitAsync("document.querySelector('#messages > article') === __metadataCachedArticle",
                "reopening cached metadata reuses the original message node");
            Check(JsonSerializer.Serialize(cached.Message) == cachedOriginal &&
                await EvalAsync<bool>($$"""
                    __metadataCachedArticle.title.includes({{endedLabel}} + ' (' + {{localLabel}} + '): ' + __metadataSmoke.localTime({{observedEnd.AddDays(-1).ToUnixTimeMilliseconds()}}))
                    """), "cached reopening retains its own local observation while leaving the formal message immutable");

            _transcript.ShowConversation(chat, rows);
            await WaitAsync($"__metadataSmoke.article('{completed.Message.Id}')?.querySelector('.copy-message') != null",
                "the copy feedback check restores the current conversation");
            await EvalAsync<bool>("(() => { scrollTo(0, 0); document.activeElement?.blur(); return true; })()");
            // Move only this owned WebView's pointer away; this is not a global mouse input or a model stream.
            // 只移开此测试 WebView 内的指针；不发送全局鼠标输入，也不调用模型流。
            await DispatchMouse("mouseMoved", 0, 0);
            await EvalAsync<bool>($$"""
                (() => {
                  const copy = __metadataSmoke.article('{{completed.Message.Id}}').querySelector('.copy-message');
                  const probe = window.__metadataCopyProbe = { copy, successAt: null, resetAt: null };
                  probe.observer = new MutationObserver(() => {
                    if (copy.dataset.copyState === 'success' && probe.successAt === null) probe.successAt = performance.now();
                    else if (!copy.dataset.copyState && probe.successAt !== null) probe.resetAt = performance.now();
                  });
                  probe.observer.observe(copy, { attributes: true, attributeFilter: ['data-copy-state'] });
                  return true;
                })()
                """);
            var feedback = new TaskCompletionSource<string>(TaskCreationOptions.RunContinuationsAsynchronously);
            EventHandler<string> callback = (_, text) => feedback.TrySetResult(text);
            _transcript.ActionFeedbackRequested += callback;
            try
            {
                Check(await EvalAsync<bool>("(() => { __metadataCopyProbe.copy.click(); return !__metadataCopyProbe.copy.dataset.copyState; })()"),
                    "copying a message with time metadata waits for the real native clipboard acknowledgement");
                Check(await feedback.Task.WaitAsync(TimeSpan.FromSeconds(5)) == UiText.Get("已复制"),
                    "the native host acknowledges the copied original message");
                await WaitAsync("__metadataCopyProbe.copy.dataset.copyState === 'success' && __metadataCopyProbe.successAt !== null",
                    "successful clipboard acknowledgement replaces the copy icon with its checkmark");
                Check(await Clipboard.GetContent().GetTextAsync() == copiedMarkdown,
                    "native message copy preserves exact Markdown, Unicode, TeX and indentation without hover metadata");
                await WaitAsync("""
                    (() => {
                      const copy = __metadataCopyProbe.copy, article = copy.closest('article'), actions = copy.closest('.message-actions');
                      return !article.matches(':hover') && !article.matches(':focus-within')
                        && getComputedStyle(actions).opacity > 0.95 && copy.querySelector('svg path')?.getAttribute('d') === 'm5 12 4 4L19 6';
                    })()
                    """, "the actual checkmark remains visible with neither mouse hover nor focus after the acknowledgement");
                await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), "kynxa-transcript-message-copy-feedback.png"));
                await WaitAsync("__metadataCopyProbe.resetAt !== null", "copy feedback resets after its short acknowledgement interval");
                Check(await EvalAsync<bool>("__metadataCopyProbe.resetAt - __metadataCopyProbe.successAt >= 2000 && __metadataCopyProbe.resetAt - __metadataCopyProbe.successAt <= 3500 && !!__metadataCopyProbe.copy.querySelector('svg rect')"),
                    "the approximately 2200ms feedback interval restores the normal copy SVG instead of leaving a permanent checkmark");
                await WaitAsync("getComputedStyle(__metadataCopyProbe.copy.closest('.message-actions')).opacity < 0.01",
                    "unhovered actions return to their quiet state once copy feedback expires");
            }
            finally { _transcript.ActionFeedbackRequested -= callback; }
            _metrics["messageMetadata"] = await EvalAsync<JsonElement>("({titles:[...document.querySelectorAll('#messages > article')].map(article => article.title),copyFeedbackMs:__metadataCopyProbe.resetAt-__metadataCopyProbe.successAt})");
            Check(completed.Message.Content == copiedMarkdown && JsonSerializer.Serialize(cached.Message) == cachedOriginal,
                "time presentation and acknowledged copy leave original message sources unchanged");
        }
        finally
        {
            await EvalAsync<bool>("(() => { window.__metadataCopyProbe?.observer.disconnect(); return true; })()");
            _transcript.ClearSelection();
            UiText.Initialize(language);
        }
    }
}
