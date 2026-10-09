using System.Text.Json;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;

namespace TranscriptUiSmoke;

public partial class App
{
    private async Task CheckMessageTimeCacheAsync()
    {
        string language = UiText.Language;
        string temporaryRoot = Path.Combine(Path.GetTempPath(), "kynxa-message-time-cache-" + Guid.NewGuid().ToString("N"));
        string desktopRoot = Path.Combine(temporaryRoot, "Desktop");
        string otherDesktopRoot = Path.Combine(temporaryRoot, "OtherDesktop");
        var chat = Guid.NewGuid();
        var otherChat = Guid.NewGuid();
        var created = new DateTimeOffset(2026, 4, 18, 2, 3, 4, TimeSpan.Zero);
        var ended = created.AddMinutes(2).AddSeconds(7);
        var otherEnded = ended.AddMinutes(1);
        var reply = new ChatMessageState
        {
            Role = "assistant", Status = "completed", Content = "CACHE_RESTART_REPLY 本机缓存保留原始回复。\n\n$x^2$",
            CreatedAt = created, DurationMs = 3456, Provider = "synthetic-provider", Model = "synthetic-model",
            Reasoning = "SYNTHETIC_PRIVATE_REASONING"
        };
        string originalMessage = JsonSerializer.Serialize(reply);
        ChatMessageState Reload() => JsonSerializer.Deserialize<ChatMessageState>(originalMessage)!;
        try
        {
            Directory.CreateDirectory(temporaryRoot);
            UiText.Initialize("zh-CN");
            _transcript.ClearSelection();
            MessageTimePresentation.ConfigureCache(null);
            MessageTimePresentation.ConfigureCache(desktopRoot);
            MessageTimePresentation.RecordEnd(chat, reply, ended);
            MessageTimePresentation.RecordEnd(chat, reply, ended.AddHours(1));
            Check(await MessageTimePresentation.SaveAsync(chat, reply), "the first observed terminal instant is saved to the isolated local display cache");
            await MessageTimePresentation.FlushAsync();
            Check(MessageTimePresentation.GetEnd(chat, reply) == ended && JsonSerializer.Serialize(reply) == originalMessage,
                "saving preserves the first observation and all authoritative message fields");

            // Reconfigure through null to discard process memory without removing the files, as a restart would.
            // 经由 null 重新配置，只清空进程内存而保留文件，模拟关闭后重新启动。
            RestartMessageTimeCache(desktopRoot);
            var reopened = Reload();
            Check(!ReferenceEquals(reply, reopened) && MessageTimePresentation.GetEnd(chat, reopened) is null,
                "the restart fixture starts with a fresh deserialized message and no in-memory observation");
            _transcript.ShowConversation(chat, [new ConversationMessageViewModel(chat, reopened)]);
            await WaitForCachedEndAsync(chat, reopened.Id, ended, "opening the reloaded conversation restores its completed time in the real WebView2 DOM");
            Check(MessageTimePresentation.GetEnd(chat, reopened) == ended && JsonSerializer.Serialize(reopened) == originalMessage,
                "automatic transcript restore recovers only presentation metadata and leaves formal serialization exact");
            Check(await EvalAsync<bool>($$"""
                (() => {
                  const article = document.querySelector('article[data-message-id="{{reopened.Id}}"]');
                  const time = article.querySelector('time.message-time');
                  return !article.hasAttribute('title') && !article.hasAttribute('aria-description')
                    && !time.hasAttribute('title') && article.querySelector('.copy-message').nextElementSibling === time
                    && time.hasAttribute('data-copy-ignore') && getComputedStyle(time).userSelect === 'none';
                })()
                """), "restored time stays beside copy, outside selection, without reintroducing message hover popups");
            string preview = Path.Combine(temporaryRoot, "message-time-restored.png");
            await CaptureViewportAsync(preview);
            _metrics["messageTimeCacheRestoredPreview"] = preview;

            var foreignConversation = Reload();
            await MessageTimePresentation.RestoreAsync(otherChat, [foreignConversation]);
            Check(MessageTimePresentation.GetEnd(otherChat, foreignConversation) is null,
                "the same message ID in a different conversation cannot inherit another conversation's time");
            _transcript.ShowConversation(otherChat, [new ConversationMessageViewModel(otherChat, foreignConversation)]);
            await WaitForUnknownEndAsync(otherChat, foreignConversation.Id,
                "a conversation with the same message ID renders an unknown end rather than a foreign timestamp");

            RestartMessageTimeCache(otherDesktopRoot);
            var foreignRoot = Reload();
            await MessageTimePresentation.RestoreAsync(chat, [foreignRoot]);
            Check(MessageTimePresentation.GetEnd(chat, foreignRoot) is null,
                "changing the configured desktop directory isolates the local end-time cache");
            _transcript.ShowConversation(chat, [new ConversationMessageViewModel(chat, foreignRoot)]);
            await WaitForUnknownEndAsync(chat, foreignRoot.Id, "the new data root does not display a cached time from the previous root");

            RestartMessageTimeCache(desktopRoot);
            var edited = Reload();
            edited.Content += " EDITED_AFTER_CACHE";
            var failed = Reload();
            failed.Status = "error";
            failed.Error = "Synthetic terminal state change.";
            var active = Reload();
            active.Status = "streaming";
            var user = Reload();
            user.Role = "user";
            var remoteClockOffset = Reload();
            remoteClockOffset.CreatedAt = ended.AddSeconds(1);
            string remoteClockMessage = JsonSerializer.Serialize(remoteClockOffset);
            await MessageTimePresentation.RestoreAsync(chat, [edited, failed, active, user, remoteClockOffset]);
            Check(MessageTimePresentation.GetEnd(chat, edited) is null && MessageTimePresentation.GetEnd(chat, failed) is null,
                "changed final content and changed terminal status do not reuse a previous attempt's observed end");
            Check(MessageTimePresentation.GetEnd(chat, active) is null && MessageTimePresentation.GetEnd(chat, user) is null,
                "streaming replies and user messages are never restored as completed ends");
            Check(MessageTimePresentation.GetEnd(chat, remoteClockOffset) == ended &&
                JsonSerializer.Serialize(remoteClockOffset) == remoteClockMessage,
                "a legitimate gateway creation-clock offset preserves the original locally observed end and authoritative fields");
            _transcript.ShowConversation(chat, [new ConversationMessageViewModel(chat, remoteClockOffset)]);
            await WaitForCachedEndAsync(chat, remoteClockOffset.Id, ended,
                "a gateway creation instant later than the local receipt still restores the original ISO end in the footer");
            var invalidLiveObservation = Reload();
            MessageTimePresentation.RecordEnd(chat, invalidLiveObservation, created.AddSeconds(-1));
            Check(MessageTimePresentation.GetEnd(chat, invalidLiveObservation) is null,
                "a newly observed live end earlier than its own local creation is still rejected");
            _transcript.ShowConversation(chat, [new ConversationMessageViewModel(chat, active)]);
            await WaitAsync($$"""
                window.transcriptState().conversationId === '{{chat}}'
                && document.querySelector('article[data-message-id="{{active.Id}}"] time.message-time')?.textContent === '正在生成'
                && !document.querySelector('article[data-message-id="{{active.Id}}"] time.message-time').hasAttribute('datetime')
                """, "the real streaming footer hides the cached completed time and shows generation instead");

            // Reusing both ID and text after explicit retry must replace the old attempt's end.
            // 明确重试后即使 ID 和正文相同，也必须记录新一轮结束时间。
            var retried = Reload();
            await MessageTimePresentation.RestoreAsync(chat, [retried]);
            Check(MessageTimePresentation.GetEnd(chat, retried) == ended, "the retry fixture starts from the previously restored completion");
            Check(await MessageTimePresentation.ForgetAsync(chat, retried) && MessageTimePresentation.GetEnd(chat, retried) is null,
                "retry invalidation removes both the persisted and live observations for that message");
            retried.Status = "streaming";
            MessageTimePresentation.RecordEnd(chat, retried, ended.AddMinutes(4));
            Check(MessageTimePresentation.GetEnd(chat, retried) is null, "a premature retry observation cannot end an active reply");
            retried.Status = "completed";
            var retriedEnd = ended.AddMinutes(5);
            MessageTimePresentation.RecordEnd(chat, retried, retriedEnd);
            Check(await MessageTimePresentation.SaveAsync(chat, retried), "the completed retry saves its new observation even with identical final content");
            await MessageTimePresentation.FlushAsync();
            RestartMessageTimeCache(desktopRoot);
            var retriedReloaded = Reload();
            _transcript.ShowConversation(chat, [new ConversationMessageViewModel(chat, retriedReloaded)]);
            await WaitForCachedEndAsync(chat, retriedReloaded.Id, retriedEnd, "a second restart restores the new retry's end instead of the original attempt");
            Check(JsonSerializer.Serialize(retriedReloaded) == originalMessage,
                "retry metadata remains outside formal message serialization");

            var otherReply = Reload();
            MessageTimePresentation.RecordEnd(otherChat, otherReply, otherEnded);
            Check(await MessageTimePresentation.SaveAsync(otherChat, otherReply), "another conversation stores its own observation under the same message ID");
            await MessageTimePresentation.FlushAsync();
            Check(await MessageTimePresentation.RemoveConversationAsync(chat), "deleting a conversation removes its own cached end-time records");
            var deletedReload = Reload();
            var retainedReload = Reload();
            await MessageTimePresentation.RestoreAsync(chat, [deletedReload]);
            await MessageTimePresentation.RestoreAsync(otherChat, [retainedReload]);
            Check(MessageTimePresentation.GetEnd(chat, deletedReload) is null &&
                MessageTimePresentation.GetEnd(otherChat, retainedReload) == otherEnded,
                "conversation deletion leaves another conversation's cache intact and does not restore deleted records");
            Check(MessageTimePresentation.GetEnd(chat, retriedReloaded) == retriedEnd,
                "the live deleted message retains its observation for the existing undo action");
            Check(await MessageTimePresentation.SaveAsync(chat, retriedReloaded), "undo can resave the original live message's display metadata");
            await MessageTimePresentation.FlushAsync();
            RestartMessageTimeCache(desktopRoot);
            var undoReload = Reload();
            await MessageTimePresentation.RestoreAsync(chat, [undoReload]);
            Check(MessageTimePresentation.GetEnd(chat, undoReload) == retriedEnd,
                "undo really rewrites the removed record so another process restart can recover it");

            // A historical chat may be deleted from the sidebar without ever opening its transcript.
            // 历史聊天可能从侧栏直接删除，其消息从未在当前进程的聊天区中打开。
            var unopenedChat = Guid.NewGuid();
            var unopenedReply = Reload();
            unopenedReply.Id = Guid.NewGuid();
            unopenedReply.Content = "UNOPENED_HISTORY_CACHE_REPLY";
            string unopenedMessage = JsonSerializer.Serialize(unopenedReply);
            var unopenedEnd = ended.AddMinutes(10);
            MessageTimePresentation.RecordEnd(unopenedChat, unopenedReply, unopenedEnd);
            Check(await MessageTimePresentation.SaveAsync(unopenedChat, unopenedReply),
                "the unopened historical conversation fixture has a previously saved local end");
            await MessageTimePresentation.FlushAsync();
            RestartMessageTimeCache(desktopRoot);
            var unopenedUndoObject = JsonSerializer.Deserialize<ChatMessageState>(unopenedMessage)!;
            Check(MessageTimePresentation.GetEnd(unopenedChat, unopenedUndoObject) is null,
                "a historical message not opened in this process has no live observation before deletion preparation");
            await MessageTimePresentation.RestoreAsync(unopenedChat, [unopenedUndoObject]);
            Check(MessageTimePresentation.GetEnd(unopenedChat, unopenedUndoObject) == unopenedEnd,
                "deletion preparation loads cached metadata for an unopened historical chat into its undo object");
            Check(await MessageTimePresentation.RemoveConversationAsync(unopenedChat),
                "the unopened historical chat's persisted observations can be deleted");
            var unopenedDeleted = JsonSerializer.Deserialize<ChatMessageState>(unopenedMessage)!;
            await MessageTimePresentation.RestoreAsync(unopenedChat, [unopenedDeleted]);
            Check(MessageTimePresentation.GetEnd(unopenedChat, unopenedDeleted) is null,
                "the unopened chat's deleted metadata is absent for a new history object");
            Check(await MessageTimePresentation.SaveAsync(unopenedChat, unopenedUndoObject),
                "undo can resave an unopened history object's preloaded observation");
            await MessageTimePresentation.FlushAsync();
            RestartMessageTimeCache(desktopRoot);
            var unopenedRestored = JsonSerializer.Deserialize<ChatMessageState>(unopenedMessage)!;
            await MessageTimePresentation.RestoreAsync(unopenedChat, [unopenedRestored]);
            Check(MessageTimePresentation.GetEnd(unopenedChat, unopenedRestored) == unopenedEnd &&
                JsonSerializer.Serialize(unopenedRestored) == unopenedMessage,
                "undo of an unopened historical chat survives restart without changing formal history");

            // Open two chats synchronously while disk reads are pending; only the latest may reach the document.
            // 文件读取尚未完成时连续打开两个聊天，只有最后一个聊天能更新当前文档。
            RestartMessageTimeCache(desktopRoot);
            var slowRows = Enumerable.Range(0, 33).Select(index => index == 0
                ? new ConversationMessageViewModel(chat, Reload())
                : Message(chat, "assistant", "RAPID_CACHE_OLD_" + index)).ToArray();
            var current = Reload();
            _transcript.ShowConversation(chat, slowRows);
            _transcript.ShowConversation(otherChat, [new ConversationMessageViewModel(otherChat, current)]);
            await WaitForCachedEndAsync(otherChat, current.Id, otherEnded,
                "rapid navigation restores the current conversation's time rather than the previous conversation's time");
            await MessageTimePresentation.RestoreAsync(chat, slowRows.Select(row => row.Message).ToArray());
            await Task.Delay(120);
            Check(await EvalAsync<bool>($$"""
                window.transcriptState().conversationId === '{{otherChat}}'
                && document.querySelectorAll('#messages > article').length === 1
                && !document.getElementById('messages').textContent.includes('RAPID_CACHE_OLD_')
                && document.querySelector('time.message-time').dateTime === new Date({{otherEnded.ToUnixTimeMilliseconds()}}).toISOString()
                """), "late cache reads cannot replace the current conversation's DOM or timestamp");

            string blockedRoot = Path.Combine(temporaryRoot, "BlockedDesktopFile");
            await File.WriteAllTextAsync(blockedRoot, "A synthetic file blocks directory creation.");
            RestartMessageTimeCache(blockedRoot);
            var unwritable = Reload();
            MessageTimePresentation.RecordEnd(chat, unwritable, ended);
            Check(!await MessageTimePresentation.SaveAsync(chat, unwritable), "an unwritable cache returns failure without throwing into model or UI handling");
            await MessageTimePresentation.FlushAsync();
            Check(MessageTimePresentation.GetEnd(chat, unwritable) == ended && JsonSerializer.Serialize(unwritable) == originalMessage,
                "a cache write failure preserves the current in-memory time and formal message state");
            _transcript.ShowConversation(chat, [new ConversationMessageViewModel(chat, unwritable)]);
            await WaitForCachedEndAsync(chat, unwritable.Id, ended, "a cache write failure does not remove the visible time from the active reply");
            _metrics["messageTimeCache"] = new
            {
                root = temporaryRoot, conversationId = chat, otherConversationId = otherChat, messageId = reply.Id,
                firstEnd = ended, retryEnd = retriedEnd, otherEnd = otherEnded,
                formalSerializationPreserved = JsonSerializer.Serialize(unwritable) == originalMessage,
                currentDom = await EvalAsync<JsonElement>("({state:window.transcriptState(),text:document.querySelector('time.message-time').textContent,dateTime:document.querySelector('time.message-time').dateTime})")
            };
        }
        finally
        {
            await MessageTimePresentation.FlushAsync();
            MessageTimePresentation.ConfigureCache(null);
            UiText.Initialize(language);
            _transcript.ClearSelection();
        }
    }

    private static void RestartMessageTimeCache(string desktopDirectory)
    {
        MessageTimePresentation.ConfigureCache(null);
        MessageTimePresentation.ConfigureCache(desktopDirectory);
    }

    private Task WaitForCachedEndAsync(Guid conversationId, Guid messageId, DateTimeOffset ended, string description) =>
        WaitAsync($$"""
            window.transcriptState().conversationId === '{{conversationId}}'
            && document.querySelector('article[data-message-id="{{messageId}}"] time.message-time')?.dateTime === new Date({{ended.ToUnixTimeMilliseconds()}}).toISOString()
            && document.querySelector('article[data-message-id="{{messageId}}"] time.message-time').textContent.startsWith('回复完成时间：')
            """, description);

    private Task WaitForUnknownEndAsync(Guid conversationId, Guid messageId, string description) =>
        WaitAsync($$"""
            window.transcriptState().conversationId === '{{conversationId}}'
            && document.querySelector('article[data-message-id="{{messageId}}"] time.message-time')?.textContent === '回复完成时间：未记录'
            && !document.querySelector('article[data-message-id="{{messageId}}"] time.message-time').hasAttribute('datetime')
            """, description);
}
