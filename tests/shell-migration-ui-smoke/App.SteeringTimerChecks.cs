using System.Collections;
using System.Diagnostics;
using System.Reflection;
using System.Text;
using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.Views;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop;

public partial class App
{
    private async Task CheckSteeringTimerAsync()
    {
        var chat = ShellField<List<ProjectChatState>>("_standaloneChats").Single(item => item.Id == _gateway.StandaloneChatId);
        var recent = _shell.ViewModel.RecentConversations.Single(item => item.Id == chat.Id);
        Call("SetPrimaryMode", true, true);
        Call("SelectStandaloneChat", recent);
        await SettleAsync();
        var originalIds = chat.Messages.Select(message => message.Id).ToHashSet();
        var replies = ShellField<IDictionary>("_pendingReplies");
        // Drive production steering and the real transcript using only fixture-owned pending messages; no stream is submitted.
        // 仅以内存夹具消息驱动生产引导及真实转录，不提交模型流或正式任务。
        var pending = Call("BeginPendingReply", chat.Id, "Synthetic timer continuity", "fixture-provider", "fixture-model", "ask", null)!;
        var type = pending.GetType();
        var taskId = Guid.NewGuid();
        type.GetProperty("TaskId")!.SetValue(pending, taskId);
        var initial = (ChatMessageState)type.GetProperty("Message")!.GetValue(pending)!;
        long origin = Stopwatch.GetTimestamp() - 120 * Stopwatch.Frequency;
        initial.GenerationStartedTimestamp = origin;
        ((StringBuilder)type.GetProperty("Content")!.GetValue(pending)!).Append("STEERING_EARLIER_OUTPUT");
        Call("FlushReply", pending, true);
        var transcript = Element<ConversationTranscript>("ConversationMessages");
        try
        {
            var prompt = new QueuedPrompt(Guid.NewGuid(), "Keep the task and change its direction", null, null, "ask");
            Call("PrepareSteeringReply", chat.Id, prompt, pending);
            var continued = (ChatMessageState)type.GetProperty("Message")!.GetValue(pending)!;
            Check(continued.Id == prompt.ReplyMessageId && continued.Status == "streaming" &&
                continued.GenerationStartedTimestamp == origin && initial.Status == "interrupted" &&
                initial.Content == "STEERING_EARLIER_OUTPUT" && Equals(type.GetProperty("TaskId")!.GetValue(pending), taskId),
                "local guidance transfers the running task's existing clock while retaining earlier output and task identity");
            string Clock(Guid id) => $"document.querySelector('article[data-message-id=\"{id:D}\"] .message-elapsed')";
            await WaitForSteeringTimerDomAsync(transcript,
                $"(() => {{ const clock = {Clock(continued.Id)}; return clock && !clock.hidden && clock.dataset.mode === 'live' && clock.textContent.includes('2分钟') && transcriptState().liveElapsedCount === 1; }})()",
                "the continued reply displays its two-minute live clock before another model segment arrives");
            Check(type.GetProperty("WaitingReplyAnchorId")!.GetValue(pending) is Guid waiting && waiting == continued.Id,
                "waiting for guidance acceptance preserves the active clock rather than presenting a finished task");

            var acknowledged = new ChatStreamEvent("reply_anchor", chat.Id, initial.Id, DateTimeOffset.UtcNow,
                ReplyMessageId: continued.Id, UserMessageId: prompt.Id, SupersededReplyMessageId: initial.Id);
            Call("AcceptReplyAnchor", pending, acknowledged);
            var remoteUser = new ChatMessageState { Role = "user", Content = "Synthetic second guidance" };
            chat.Messages.Add(remoteUser);
            var remoteReplyId = Guid.NewGuid();
            var target = new ChatMessageState { Id = remoteReplyId, Role = "assistant", Status = "streaming",
                GenerationStartedTimestamp = Stopwatch.GetTimestamp(), InReplyToUserMessageId = remoteUser.Id,
                OriginUserMessageId = (Guid)type.GetProperty("UserMessageId")!.GetValue(pending)! };
            chat.Messages.Add(target);
            Call("UpdateConversationPresentation", false);
            await WaitForSteeringTimerDomAsync(transcript,
                $"(() => {{ const clock = {Clock(target.Id)}; return clock && clock.dataset.mode === 'live' && !clock.textContent.includes('2分钟'); }})()",
                "the existing target row has a later live origin before its task anchor arrives");
            Call("AcceptReplyAnchor", pending, acknowledged with { ReplyMessageId = remoteReplyId,
                UserMessageId = remoteUser.Id, SupersededReplyMessageId = continued.Id });
            Call("AcceptReplyAnchor", pending, acknowledged with { ReplyMessageId = remoteReplyId,
                UserMessageId = remoteUser.Id, SupersededReplyMessageId = continued.Id });
            var remote = (ChatMessageState)type.GetProperty("Message")!.GetValue(pending)!;
            typeof(ShellPage).GetMethod("StartReplyTimer", BindingFlags.Static | BindingFlags.NonPublic)!.Invoke(null, [pending]);
            Check(remote.Id == remoteReplyId && remote.Status == "streaming" && remote.GenerationStartedTimestamp == origin &&
                remote.OriginUserMessageId == target.OriginUserMessageId && Equals(type.GetProperty("TaskId")!.GetValue(pending), taskId),
                "later target timestamps, repeated server anchors and subsequent model output cannot reset the cumulative timing origin");
            await WaitForSteeringTimerDomAsync(transcript,
                $"(() => {{ const clock = {Clock(remote.Id)}; return clock && !clock.hidden && clock.dataset.mode === 'live' && clock.textContent.includes('2分钟') && transcriptState().liveElapsedCount === 1; }})()",
                "a server-driven guidance anchor keeps exactly one live cumulative clock");

            ((StringBuilder)type.GetProperty("Content")!.GetValue(pending)!).Append("STEERING_FINAL_OUTPUT");
            remote.Status = "completed"; remote.DurationMs = 183000;
            Call("FlushReply", pending, true); replies.Remove(chat.Id);
            Call("UpdateConversationPresentation", false);
            await WaitForSteeringTimerDomAsync(transcript,
                $"(() => {{ const clock = {Clock(remote.Id)}; return clock && clock.dataset.mode === 'final' && clock.textContent === '用时 3分钟3秒' && !transcriptState().elapsedTimerActive && transcriptState().liveElapsedCount === 0; }})()",
                "real completion freezes the canonical gateway total and removes the live interval");
            Check(remote.DurationMs == 183000 && remote.Status == "completed" && initial.Content == "STEERING_EARLIER_OUTPUT",
                "finishing guidance retains the full duration and earlier progress without another request or repeated operation");
        }
        finally
        {
            replies.Remove(chat.Id);
            chat.Messages.RemoveAll(message => !originalIds.Contains(message.Id));
            ((CancellationTokenSource)type.GetProperty("Cancellation")!.GetValue(pending)!).Dispose();
            ShellField<DispatcherTimer?>("_replyRefreshTimer")?.Stop();
            Call("UpdateConversationPresentation", false);
        }
    }

    private async Task WaitForSteeringTimerDomAsync(ConversationTranscript transcript, string source, string description)
    {
        DateTime deadline = DateTime.UtcNow.AddSeconds(10);
        while (await transcript.Browser.ExecuteScriptAsync(source) != "true")
        {
            if (_unhandled is not null) throw new InvalidOperationException(description, _unhandled);
            if (_gateway.Failure is not null) throw new InvalidOperationException(description, _gateway.Failure);
            if (DateTime.UtcNow > deadline) throw new TimeoutException(description);
            await Task.Delay(30);
        }
    }
}
