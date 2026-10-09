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
        var originalSize = _window!.AppWindow.Size;
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
                  // Copy toolbars localize independently; compare prose/source plus node identity.
                  // 复制栏独立更新语言；正文和源码应保留原文以及原节点身份。
                  window.__metadataBodySource = body => {
                    const clone=body.cloneNode(true); clone.querySelectorAll('[data-copy-ignore]').forEach(node=>node.remove());
                    return clone.innerHTML;
                  };
                  window.__metadataCachedBodyNode = __metadataCachedArticle.querySelector('.message-body');
                  window.__metadataCachedBody = __metadataBodySource(__metadataCachedBodyNode);
                  window.__metadataSmoke = {
                    article: id => document.querySelector(`article[data-message-id="${id}"]`),
                    time(id) { return this.article(id).querySelector('time.message-time'); },
                    localTime: value => {
                      const d = new Date(value), pad = number => String(number).padStart(2, '0');
                      return `${d.getFullYear()}/${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
                    },
                    clean(article) { return !article.hasAttribute('title') && !article.hasAttribute('aria-description'); },
                    geometry() {
                      const rect = element => {
                        const box = element.getBoundingClientRect();
                        return { left: box.left, right: box.right, top: box.top, bottom: box.bottom, width: box.width, height: box.height };
                      };
                      return { viewport: innerWidth, pageWidth: document.documentElement.scrollWidth,
                        rows: [...document.querySelectorAll('#messages > article')].map(article => {
                          const actions = article.querySelector('.message-actions'), copy = actions.querySelector('.copy-message');
                          const time = actions.querySelector('.message-time'), retry = actions.querySelector('.retry');
                          const style = getComputedStyle(actions);
                          const timeStyle = getComputedStyle(time), bodyStyle = getComputedStyle(article.querySelector('.message-body'));
                          return { role: article.dataset.role, text: time.textContent,
                            actions: rect(actions), copy: rect(copy), time: rect(time),
                            timeTypography: { fontFamily: timeStyle.fontFamily, fontVariantNumeric: timeStyle.fontVariantNumeric },
                            bodyTypography: { fontFamily: bodyStyle.fontFamily, fontVariantNumeric: bodyStyle.fontVariantNumeric },
                            visible: style.opacity > 0.95 && style.visibility === 'visible' && style.display !== 'none',
                            order: copy.nextElementSibling === time && time.nextElementSibling === retry,
                            ignored: time.hasAttribute('data-copy-ignore') && getComputedStyle(time).userSelect === 'none',
                            noHover: this.clean(article) && !time.hasAttribute('title') };
                        }) };
                    },
                    copyPosition(copy) {
                      const actions = copy.closest('.message-actions').getBoundingClientRect();
                      const copyBox = copy.getBoundingClientRect(), time = copy.nextElementSibling;
                      const timeBox = time.getBoundingClientRect();
                      return { copyLeft: copyBox.left - actions.left, copyTop: copyBox.top - actions.top,
                        copyWidth: copyBox.width, timeLeft: timeBox.left - actions.left, timeTop: timeBox.top - actions.top,
                        timeWidth: timeBox.width, timeText: time.textContent, dateTime: time.getAttribute('datetime') };
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
            var cancelledHistory = Message(chat, "assistant", "METADATA_CANCELLED_HISTORY", "interrupted");
            var failedHistory = Message(chat, "assistant", "METADATA_FAILED_HISTORY", "error");
            MessageTimePresentation.RecordEnd(user.Message, observedEnd);
            MessageTimePresentation.RecordEnd(completed.Message, observedEnd);
            Check(MessageTimePresentation.GetEnd(user.Message) is null && MessageTimePresentation.GetEnd(completed.Message) is null &&
                MessageTimePresentation.GetEnd(history.Message) is null,
                "user, active generation and unobserved historical completion do not fabricate an end time");
            var rows = new List<ConversationMessageViewModel> { user, completed, history, unknown, cancelled, failed, cancelledHistory, failedHistory };
            _transcript.ShowConversation(chat, rows);
            await WaitAsync("document.querySelectorAll('#messages > article').length === 8 && __transcriptSmoke.bodyText().includes('METADATA_FAILED_HISTORY')",
                "synthetic user, streaming and historical message states render in the production transcript");
            Check(await EvalAsync<bool>($$"""
                (() => {
                  const s = __metadataSmoke, sent = s.time('{{user.Message.Id}}'), active = s.time('{{completed.Message.Id}}');
                  return sent.textContent === '发送时间：' + s.localTime({{user.Message.CreatedAt.ToUnixTimeMilliseconds()}})
                    && sent.dateTime === new Date({{user.Message.CreatedAt.ToUnixTimeMilliseconds()}}).toISOString()
                    && active.textContent === '正在生成' && !active.hasAttribute('datetime')
                    && ['{{history.Message.Id}}', '{{unknown.Message.Id}}'].every(id =>
                      s.time(id).textContent === '回复完成时间：未记录' && !s.time(id).hasAttribute('datetime'))
                    && ['{{cancelledHistory.Message.Id}}', '{{failedHistory.Message.Id}}'].every(id =>
                      s.time(id).textContent === '回复结束时间：未记录' && !s.time(id).hasAttribute('datetime'))
                    && [...document.querySelectorAll('#messages > article')].every(article => s.clean(article));
                })()
                """), "persistent times distinguish sending, generating and unknown terminal states without a message hover popup or invented dateTime");

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
                __metadataSmoke.time('{{completed.Message.Id}}').textContent === '回复完成时间：' + __metadataSmoke.localTime({{observedEnd.ToUnixTimeMilliseconds()}})
                && __metadataSmoke.time('{{cancelled.Message.Id}}').textContent === '回复结束时间：' + __metadataSmoke.localTime({{observedEnd.AddSeconds(1).ToUnixTimeMilliseconds()}})
                && __metadataSmoke.time('{{failed.Message.Id}}').textContent === '回复结束时间：' + __metadataSmoke.localTime({{observedEnd.AddSeconds(2).ToUnixTimeMilliseconds()}})
                """, "successful, cancelled and failed replies display actual terminal instants with success reserved for completed replies");
            Check(MessageTimePresentation.GetEnd(completed.Message) == observedEnd && JsonSerializer.Serialize(completed.Message) == completedOriginal &&
                await EvalAsync<bool>($$"""
                    (() => {
                      const s = __metadataSmoke;
                      return s.time('{{completed.Message.Id}}').dateTime === new Date({{observedEnd.ToUnixTimeMilliseconds()}}).toISOString()
                        && s.time('{{cancelled.Message.Id}}').dateTime === new Date({{observedEnd.AddSeconds(1).ToUnixTimeMilliseconds()}}).toISOString()
                        && s.time('{{failed.Message.Id}}').dateTime === new Date({{observedEnd.AddSeconds(2).ToUnixTimeMilliseconds()}}).toISOString()
                        && !s.time('{{completed.Message.Id}}').textContent.includes(s.localTime({{created.AddMilliseconds(completed.Message.DurationMs).ToUnixTimeMilliseconds()}}))
                        && s.time('{{history.Message.Id}}').textContent === '回复完成时间：未记录'
                        && [...document.querySelectorAll('.message-time')].every(time => !time.textContent.includes('本机记录'));
                    })()
                    """), "ISO dateTime uses the first observed end instead of creation plus duration and keeps source serialization unchanged");

            await EvalAsync<bool>($$"""
                (() => {
                  window.__metadataUserBody = __metadataSmoke.article('{{user.Message.Id}}').querySelector('.message-body');
                  window.__metadataUserHtml = __metadataUserBody.innerHTML;
                  return true;
                })()
                """);
            user.Message.CreatedAt = user.Message.CreatedAt.AddDays(1);
            user.Refresh();
            await WaitAsync($$"""
                __metadataSmoke.time('{{user.Message.Id}}').textContent === '发送时间：' + __metadataSmoke.localTime({{user.Message.CreatedAt.ToUnixTimeMilliseconds()}})
                && __metadataSmoke.time('{{user.Message.Id}}').dateTime === new Date({{user.Message.CreatedAt.ToUnixTimeMilliseconds()}}).toISOString()
                """, "changing only the user's CreatedAt refreshes the persistent sending time and ISO dateTime");
            Check(await EvalAsync<bool>($$"""
                __metadataUserBody === __metadataSmoke.article('{{user.Message.Id}}').querySelector('.message-body')
                && __metadataUserBody.innerHTML === __metadataUserHtml
                """), "updating only the timestamp preserves the original message body DOM");

            var measurements = new List<JsonElement>();
            foreach (int width in new[] { 980, 480 })
            {
                _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(width, 900));
                await Task.Delay(220);
                await EvalAsync<bool>("(() => { scrollTo(0, 0); document.activeElement?.blur(); return true; })()");
                await DispatchMouse("mouseMoved", 0, 0);
                var geometry = await EvalAsync<JsonElement>("__metadataSmoke.geometry()");
                measurements.Add(geometry);
                _metrics["messageTimeGeometry"] = measurements;
                Check(geometry.GetProperty("pageWidth").GetDouble() <= geometry.GetProperty("viewport").GetDouble() + 1,
                    "persistent message times do not widen the page at native width " + width);
                foreach (var row in geometry.GetProperty("rows").EnumerateArray())
                {
                    var copyBox = row.GetProperty("copy");
                    var timeBox = row.GetProperty("time");
                    var actionsBox = row.GetProperty("actions");
                    Check(row.GetProperty("visible").GetBoolean() && row.GetProperty("order").GetBoolean() &&
                        row.GetProperty("ignored").GetBoolean() && row.GetProperty("noHover").GetBoolean() &&
                        copyBox.GetProperty("width").GetDouble() > 0 && timeBox.GetProperty("height").GetDouble() > 0 &&
                        timeBox.GetProperty("left").GetDouble() >= copyBox.GetProperty("right").GetDouble() + 6 &&
                        timeBox.GetProperty("right").GetDouble() <= actionsBox.GetProperty("right").GetDouble() + 1 &&
                        timeBox.GetProperty("top").GetDouble() < copyBox.GetProperty("bottom").GetDouble() &&
                        timeBox.GetProperty("bottom").GetDouble() > copyBox.GetProperty("top").GetDouble(),
                        row.GetProperty("role").GetString() + " time stays visible to the right of copy without hover, overlap or clipping at " + width);
                }
                await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), $"kynxa-transcript-message-time-states-{width}.png"));
            }
            _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 900));
            await Task.Delay(180);

            string selected = await EvalAsync<string>("""
                (() => {
                  const messages = document.getElementById('messages');
                  const first = __transcriptSmoke.textNode(messages, 'USER_METADATA_LITERAL');
                  const last = __transcriptSmoke.textNode(messages, 'METADATA_FAILED_HISTORY');
                  const selection = getSelection();
                  selection.removeAllRanges(); selection.setBaseAndExtent(first, 0, last, last.nodeValue.length);
                  return __transcriptSmoke.copyEvent();
                })()
                """);
            Check(selected.Contains("USER_METADATA_LITERAL") && selected.Contains("METADATA_COPY_BODY") && selected.Contains(@"\alpha") &&
                !selected.Contains("发送时间") && !selected.Contains("回复完成时间") && !selected.Contains("回复结束时间") && !selected.Contains("未记录") &&
                !selected.Contains("2026/04/"),
                "cross-message selection preserves original message and TeX content and excludes all persistent footer times");
            await EvalAsync<bool>("""
                (() => {
                  const selection = getSelection();
                  window.__metadataSelection = { text: window.transcriptSelectionText(), anchor: selection.anchorNode,
                    anchorOffset: selection.anchorOffset, focus: selection.focusNode, focusOffset: selection.focusOffset };
                  window.__metadataBodyNodes = [...document.querySelectorAll('.message-body')];
                  window.__metadataCodeNodes = [...document.querySelectorAll('.message-body pre > code')];
                  window.__metadataBodyHtml = __metadataBodyNodes.map(__metadataBodySource);
                  return true;
                })()
                """);
            UiText.Initialize("en");
            string sentLabel = JsonSerializer.Serialize(UiText.Get("发送时间"));
            string completedLabel = JsonSerializer.Serialize(UiText.Get("回复完成时间"));
            string endedLabel = JsonSerializer.Serialize(UiText.Get("回复结束时间"));
            await WaitAsync($$"""
                document.documentElement.lang === 'en'
                && __metadataSmoke.time('{{user.Message.Id}}').textContent.startsWith({{sentLabel}} + ': ')
                && __metadataSmoke.time('{{completed.Message.Id}}').textContent.startsWith({{completedLabel}} + ': ')
                && __metadataSmoke.time('{{cancelled.Message.Id}}').textContent.startsWith({{endedLabel}} + ': ')
                && __metadataCachedArticle.querySelector('.message-time').textContent.startsWith({{completedLabel}} + ': ')
                """, "active and detached cached persistent times switch to English without reopening the conversation");
            Check(await EvalAsync<bool>("""
                (() => {
                  const selection = getSelection(), saved = __metadataSelection;
                  return !__metadataCachedArticle.isConnected
                    && __metadataCachedArticle.querySelector('.message-body') === __metadataCachedBodyNode
                    && __metadataBodySource(__metadataCachedBodyNode) === __metadataCachedBody
                    && [...document.querySelectorAll('.message-body')].every((body, i) => body === __metadataBodyNodes[i] && __metadataBodySource(body) === __metadataBodyHtml[i])
                    && [...document.querySelectorAll('.message-body pre > code')].every((code, i) => code === __metadataCodeNodes[i])
                    && selection.anchorNode === saved.anchor && selection.anchorOffset === saved.anchorOffset
                    && selection.focusNode === saved.focus && selection.focusOffset === saved.focusOffset
                    && window.transcriptSelectionText() === saved.text
                    && [...document.querySelectorAll('#messages > article'), __metadataCachedArticle].every(article => __metadataSmoke.clean(article));
                })()
                """), "footer localization preserves exact selection and original current/cached body DOM without restoring hover metadata");
            _transcript.ClearSelection();
            _transcript.ShowConversation(cachedChat, [cached]);
            await WaitAsync("document.querySelector('#messages > article') === __metadataCachedArticle",
                "reopening cached metadata reuses the original message node");
            Check(JsonSerializer.Serialize(cached.Message) == cachedOriginal &&
                await EvalAsync<bool>($$"""
                    __metadataCachedArticle.querySelector('.message-time').textContent === {{completedLabel}} + ': ' + __metadataSmoke.localTime({{observedEnd.AddDays(-1).ToUnixTimeMilliseconds()}})
                    && __metadataCachedArticle.querySelector('.message-time').dateTime === new Date({{observedEnd.AddDays(-1).ToUnixTimeMilliseconds()}}).toISOString()
                    """), "cached reopening retains its own terminal observation without modifying the formal message");

            _transcript.ShowConversation(chat, rows);
            await WaitAsync($"__metadataSmoke.article('{completed.Message.Id}')?.querySelector('.copy-message') != null",
                "the copy feedback check restores the current conversation");
            await EvalAsync<bool>($$"""
                (() => {
                  __metadataSmoke.article('{{completed.Message.Id}}').querySelector('.message-actions').scrollIntoView({ block: 'center' });
                  document.activeElement?.blur(); return true;
                })()
                """);
            // Move only this owned WebView's pointer away; this is not a global mouse input or a model stream.
            // 只移开此测试 WebView 内的指针；不发送全局鼠标输入，也不调用模型流。
            await DispatchMouse("mouseMoved", 0, 0);
            await EvalAsync<bool>($$"""
                (() => {
                  const copy = __metadataSmoke.article('{{completed.Message.Id}}').querySelector('.copy-message');
                  const probe = window.__metadataCopyProbe = { copy, successAt: null, resetAt: null,
                    originalPosition: __metadataSmoke.copyPosition(copy) };
                  probe.observer = new MutationObserver(() => {
                    if (copy.dataset.copyState === 'success' && probe.successAt === null) probe.successAt = performance.now();
                    else if (!copy.dataset.copyState && probe.successAt !== null) probe.resetAt = performance.now();
                  });
                  probe.observer.observe(copy, { attributes: true, attributeFilter: ['data-copy-state'] });
                  return true;
                })()
                """);
            {
                Check(await EvalAsync<bool>("(() => { __metadataCopyProbe.copy.click(); return !__metadataCopyProbe.copy.dataset.copyState; })()"),
                    "copying a message with a persistent time waits for the real native clipboard acknowledgement");
                await WaitAsync("__metadataCopyProbe.copy.dataset.copyState === 'success' && __metadataCopyProbe.successAt !== null",
                    "successful clipboard acknowledgement replaces the copy icon with its checkmark");
                Check(await EvalAsync<bool>("__metadataCopyProbe.copy.closest('article').querySelector('.message-copy-status').textContent === __metadataCopyProbe.copy.getAttribute('aria-label') && !__metadataCopyProbe.copy.hasAttribute('title')"),
                    "message copy confirms its result locally without a floating title");
                Check(await Clipboard.GetContent().GetTextAsync() == copiedMarkdown,
                    "native message copy preserves exact Markdown, Unicode, TeX and indentation without footer metadata");
                await WaitAsync("""
                    (() => {
                      const copy = __metadataCopyProbe.copy, article = copy.closest('article'), actions = copy.closest('.message-actions');
                      return !article.matches(':hover') && !article.matches(':focus-within')
                        && getComputedStyle(actions).opacity > 0.95 && copy.querySelector('svg path')?.getAttribute('d') === 'm5 12 4 4L19 6';
                    })()
                    """, "the actual checkmark and persistent time stay visible without mouse hover or focus");
                Check(await EvalAsync<bool>("JSON.stringify(__metadataSmoke.copyPosition(__metadataCopyProbe.copy)) === JSON.stringify(__metadataCopyProbe.originalPosition)"),
                    "the copy success checkmark changes neither time text nor copy/time positions");
                await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), "kynxa-transcript-message-copy-feedback.png"));
                await WaitAsync("__metadataCopyProbe.resetAt !== null", "copy feedback resets after its short acknowledgement interval");
                Check(await EvalAsync<bool>("__metadataCopyProbe.resetAt - __metadataCopyProbe.successAt >= 2000 && __metadataCopyProbe.resetAt - __metadataCopyProbe.successAt <= 3500 && !!__metadataCopyProbe.copy.querySelector('svg rect')"),
                    "the approximately 2200ms feedback interval restores the normal copy SVG");
                Check(await EvalAsync<bool>("getComputedStyle(__metadataCopyProbe.copy.closest('.message-actions')).opacity > 0.95 && JSON.stringify(__metadataSmoke.copyPosition(__metadataCopyProbe.copy)) === JSON.stringify(__metadataCopyProbe.originalPosition)"),
                    "the normal copy icon and persistent time remain visible and stationary after feedback expires");
            }
            _metrics["messageMetadata"] = await EvalAsync<JsonElement>("({times:[...document.querySelectorAll('#messages > article')].map(article => ({text:article.querySelector('.message-time').textContent,dateTime:article.querySelector('.message-time').getAttribute('datetime'),title:article.getAttribute('title'),description:article.getAttribute('aria-description')})),copyFeedbackMs:__metadataCopyProbe.resetAt-__metadataCopyProbe.successAt})");
            Check(completed.Message.Content == copiedMarkdown && JsonSerializer.Serialize(cached.Message) == cachedOriginal,
                "persistent time presentation and acknowledged copy leave original message sources unchanged");
            await CaptureMessageTimePreviewAsync(created, observedEnd);
        }
        finally
        {
            await EvalAsync<bool>("(() => { window.__metadataCopyProbe?.observer.disconnect(); return true; })()");
            _transcript.ClearSelection();
            UiText.Initialize(language);
            _window.AppWindow.Resize(originalSize);
            await Task.Delay(150);
        }
    }

    private async Task CaptureMessageTimePreviewAsync(DateTimeOffset created, DateTimeOffset observedEnd)
    {
        var chat = Guid.NewGuid();
        var user = Message(chat, "user", "请帮我确认消息时间的显示。");
        user.Message.CreatedAt = created;
        var reply = Message(chat, "assistant", "已完成。每条消息的时间会显示在复制图标右侧。");
        reply.Message.CreatedAt = created.AddSeconds(1);
        MessageTimePresentation.RecordEnd(reply.Message, observedEnd);
        UiText.Initialize("zh-CN");
        _transcript.ShowConversation(chat, [user, reply]);
        await WaitAsync($$"""
            document.querySelectorAll('#messages > article').length === 2
            && __metadataSmoke.time('{{user.Message.Id}}').textContent.startsWith('发送时间：')
            && __metadataSmoke.time('{{reply.Message.Id}}').textContent.startsWith('回复完成时间：')
            """, "a completed two-message conversation renders the Chinese inline sending and reply-completion times");
        foreach (int width in new[] { 980, 480 })
        {
            _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(width, 700));
            await Task.Delay(180);
            await EvalAsync<bool>("(() => { scrollTo(0, 0); return true; })()");
            await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), $"kynxa-transcript-message-time-{width}.png"));
        }
        UiText.Initialize("en");
        await WaitAsync($$"""
            document.documentElement.lang === 'en'
            && __metadataSmoke.time('{{reply.Message.Id}}').textContent.startsWith({{JsonSerializer.Serialize(UiText.Get("回复完成时间"))}} + ': ')
            """, "the same completed two-message preview localizes its persistent time to English");
        _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 700));
        await Task.Delay(180);
        await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), "kynxa-transcript-message-time-en.png"));
    }
}
