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
        var pending = Call("BeginPendingReply", chat.Id, "Synthetic timer continuity", "fixture-provider", "fixture-model", "ask", null, null)!;
        var type = pending.GetType();
        var taskId = Guid.NewGuid();
        type.GetProperty("TaskId")!.SetValue(pending, taskId);
        var waitingMessages = ShellField<Dictionary<Guid, List<QueuedPrompt>>>("_queuedPrompts");
        string priorDraft = Prompt.Text;
        try
        {
            int submittedBeforeQueue = _gateway.SteeringAttempts.Count;
            Prompt.Text = "WAIT_FOR_CURRENT_REPLY";
            Check((bool)Call("TryQueuePrompt")!, "sending during execution enters the visible waiting queue");
            await Task.Delay(80);
            var guidance = waitingMessages[chat.Id].Single();
            Check(!guidance.IsTaskGuidance && _gateway.SteeringAttempts.Count == submittedBeforeQueue
                && Prompt.Text == string.Empty && Element<ConversationPromptQueue>("PromptQueue").Visibility == Visibility.Visible
                && !chat.Messages.Any(message => message.Id == guidance.Id),
                "ordinary queued input stays in its card without submitting guidance or a new user message");
            waitingMessages[chat.Id].Clear();
            type.GetProperty("TaskId")!.SetValue(pending, null);
            Prompt.Text = "QUEUE_BEFORE_REGISTRATION";
            Check((bool)Call("TryQueuePrompt")!, "a visible live request queues input before formal task registration");
            var unregistered = waitingMessages[chat.Id].Single();
            Check(!unregistered.IsTaskGuidance && _gateway.SteeringAttempts.Count == submittedBeforeQueue,
                "an unregistered live reply cannot turn queued input into an immediate request");
            waitingMessages[chat.Id].Clear();
        }
        finally { Prompt.Text = priorDraft; type.GetProperty("TaskId")!.SetValue(pending, taskId); }
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
            var completed = new TaskRecord(taskId, initial.Id.ToString("D"), "Completed race fixture", [], new(chat.Id, null),
                null, [], new(256, null), new(0, 0, 0, 0, 0, 0, 0), "completed", 1, 1, null, 0, null, 0, 0);
            _gateway.SteeringDetail = new(completed, [], [], null, []);
            replies[chat.Id] = pending;
            var completionRace = new QueuedPrompt(Guid.NewGuid(), "SEND_AFTER_NORMAL_COMPLETION", null, null, "ask")
                { IsTaskGuidance = true, TargetTaskId = taskId, TargetRequestId = initial.Id };
            Guid stableUser = completionRace.Id, stableReply = completionRace.ReplyMessageId;
            Check(!await (Task<bool>)Call("SendTaskSteeringAsync", chat.Id, completionRace)!
                && !completionRace.IsTaskGuidance && completionRace.TargetTaskId is null && completionRace.TargetRequestId is null
                && completionRace.Id == stableUser && completionRace.ReplyMessageId == stableReply,
                "a formally completed send race becomes an ordinary message with its original identities instead of blocking on task retry");
            var uncertain = new QueuedPrompt(Guid.NewGuid(), "UNCERTAIN_PREVIOUS_DELIVERY", null, null, "ask")
                { IsTaskGuidance = true, TargetTaskId = taskId, TargetRequestId = initial.Id, SteeringDeliveryUnconfirmed = true };
            Check(await (Task<bool>)Call("SendTaskSteeringAsync", chat.Id, uncertain)!
                && uncertain.IsTaskGuidance && !uncertain.SteeringDeliveryUnconfirmed
                && _gateway.LastSteeringReplay?.UserMessageId == uncertain.Id
                && _gateway.LastSteeringReplay?.ReplyMessageId == uncertain.ReplyMessageId,
                "a lost steering acknowledgement is reconciled by the same task and message identities instead of being replayed as a new chat");
            _gateway.SteeringDetail = new(completed with { State = "running", Version = 10 }, [], [], null, []);
            _gateway.ConflictNextSteering = true;
            int priorReads = _gateway.SteeringDetailReads, priorAttempts = _gateway.SteeringAttempts.Count;
            var versionRace = new QueuedPrompt(Guid.NewGuid(), "SAME_ID_RETRY_AFTER_TASK_CONFLICT", null, null, "ask")
                { IsTaskGuidance = true, TargetTaskId = taskId, TargetRequestId = initial.Id };
            Check(await (Task<bool>)Call("SendTaskSteeringAsync", chat.Id, versionRace)!
                && _gateway.SteeringDetailReads == priorReads + 2 && _gateway.SteeringAttempts.Count == priorAttempts + 2
                && _gateway.SteeringAttempts.TakeLast(2).All(attempt => attempt.UserMessageId == versionRace.Id
                    && attempt.ReplyMessageId == versionRace.ReplyMessageId && attempt.Message == versionRace.Text)
                && _gateway.SteeringAttempts[^2].ExpectedVersion == 10 && _gateway.SteeringAttempts[^1].ExpectedVersion == 11
                && versionRace.TargetTaskId == taskId && versionRace.TargetRequestId == initial.Id,
                "TASK_CONFLICT rereads and retries once on the same task, request and message identities using the fresh version");
            var held = new QueuedPrompt(Guid.NewGuid(), "EXPLICIT_GUIDANCE_RETAINS_CARD_UNTIL_ACK", null, null, "ask");
            waitingMessages[chat.Id] = [held];
            Call("RefreshPromptQueue");
            var acknowledgement = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            _gateway.SteeringAcknowledgementGate = acknowledgement;
            Task steering = (Task)Call("SteerQueuedPromptAsync", chat.Id, held)!;
            try
            {
                await WaitAsync(() => _gateway.LastSteeringReplay?.UserMessageId == held.Id,
                    "explicit guidance reaches its original task while the acknowledgement is held");
                Check(held.IsTaskGuidance && held.IsSubmitting && waitingMessages[chat.Id].Contains(held)
                    && Element<ConversationPromptQueue>("PromptQueue").Visibility == Visibility.Visible,
                    "an in-flight guidance card stays visible and disabled until its formal acknowledgement");
            }
            finally { acknowledgement.TrySetResult(); _gateway.SteeringAcknowledgementGate = null; }
            await steering;
            Check(!waitingMessages[chat.Id].Contains(held)
                && held.TargetTaskId == taskId && held.TargetRequestId == initial.Id,
                "acknowledgement removes only the accepted card and preserves the original task identity");
            replies.Remove(chat.Id);
            _gateway.SteeringDetail = null;
            Prompt.Text = "IDLE_DIRECT_CHAT";
            Check(!(bool)Call("TryQueuePrompt")! && Prompt.Text == "IDLE_DIRECT_CHAT",
                "an idle message remains an ordinary chat send without waiting for a paused task or choosing a target");
            Prompt.Text = priorDraft;
            var recoveryField = typeof(ShellPage).GetField("_runningTaskRecovery", BindingFlags.Instance | BindingFlags.NonPublic)!;
            var originalRecovery = recoveryField.GetValue(_shell);
            using var recoveryCancellation = new CancellationTokenSource();
            var recoveryRead = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            var recovering = Activator.CreateInstance(recoveryField.FieldType,
                BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic, null,
                [chat, null, recoveryCancellation], null)!;
            recoveryField.FieldType.GetProperty("ReadTask")!.SetValue(recovering, recoveryRead.Task);
            recoveryField.SetValue(_shell, recovering);
            try
            {
                Prompt.Text = "WAIT_FOR_RUNNING_TASK_RECONNECT";
                Check((bool)Call("TryQueuePrompt")! && waitingMessages[chat.Id].Count == 1
                    && !_shell.ActiveMessages.Any(row => row.Message.Content == "WAIT_FOR_RUNNING_TASK_RECONNECT"),
                    "task discovery without a local pending reply keeps input queued instead of submitting a new root");
            }
            finally
            {
                waitingMessages[chat.Id].Clear();
                recoveryField.SetValue(_shell, originalRecovery);
                recoveryRead.TrySetResult();
                Prompt.Text = priorDraft;
                Call("RefreshPromptQueue");
            }
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
