using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private readonly Dictionary<Guid, List<QueuedPrompt>> _queuedPrompts = [];
    private readonly HashSet<Guid> _drainingPromptChats = [];
    private readonly HashSet<Guid> _steeringPromptChats = [];

    private bool HasQueuedTaskActivity(Guid chatId)
    {
        if (_pendingReplies.ContainsKey(chatId) || _resumingTaskConversations.Contains(chatId)) return true;
        if (_runningTaskRecovery is not { } recovery || recovery.Chat.Id != chatId) return false;
        return recovery.ReadFailed || recovery.Attaching || recovery.ReadTask is { IsCompleted: false }
            || QueueObservedTasks(recovery).Any(task => task.ParentTaskId is null && task.State is "queued" or "running");
    }

    private static IEnumerable<KYNXA.Contracts.TaskRecord> QueueObservedTasks(RunningTaskRecovery recovery) =>
        (recovery.Tasks ?? []).Where(task => task.TaskId != recovery.Detail?.Task.TaskId)
            .Concat(recovery.Detail is { } detail ? [detail.Task] : []);

    private async Task<bool> AwaitQueueObservationAsync(Guid chatId, bool retry)
    {
        if (_runningTaskRecovery is not { } recovery || recovery.Chat.Id != chatId) return true;
        if (retry && (recovery.ReadFailed || !_pendingReplies.ContainsKey(chatId) && recovery.ReadTask is not { IsCompleted: false }))
        {
            recovery.ReadFailed = false; recovery.Finished = false; recovery.Detail = null;
            recovery.ReadTask = ReadRunningTaskObservationAsync(recovery);
        }
        if (recovery.ReadTask is { } reading) await reading;
        return !recovery.ReadFailed;
    }

    private void InitializePromptQueue()
    {
        PromptQueue.SteerRequested += async (_, prompt) =>
        {
            if (ActiveChatId is Guid chatId) await SteerQueuedPromptAsync(chatId, prompt);
        };
        PromptQueue.RemoveRequested += (_, prompt) =>
        {
            if (ActiveChatId is Guid chatId && !prompt.IsSubmitting)
            {
                QueueFor(chatId).Remove(prompt);
                foreach (var file in prompt.AttachmentDrafts) DeleteTemporaryAttachment(file);
                RefreshPromptQueue();
            }
        };
        PromptQueue.MoveRequested += (_, move) =>
        {
            if (ActiveChatId is not Guid chatId || move.Prompt.IsSubmitting) return;
            var queue = QueueFor(chatId);
            int current = queue.IndexOf(move.Prompt), target = current + move.Offset;
            if (current < 0 || target < 0 || target >= queue.Count || queue[target].IsSubmitting) return;
            queue.RemoveAt(current);
            queue.Insert(target, move.Prompt);
            RefreshPromptQueue();
        };
        InitializeUnknownOutcomeCard();
    }

    private List<QueuedPrompt> QueueFor(Guid chatId)
    {
        if (!_queuedPrompts.TryGetValue(chatId, out var queue)) _queuedPrompts[chatId] = queue = [];
        return queue;
    }

    private void ConsumeAcceptedGuidance(Guid chatId, QueuedPrompt prompt)
    {
        // Acceptance consumes the durable message pair, even if a refreshed card uses another object instance.
        // 接受回执按固定消息对消费卡片，刷新后换了对象实例也不能留下可重发副本。
        if (_queuedPrompts.TryGetValue(chatId, out var queue))
            queue.RemoveAll(item => item.Id == prompt.Id && item.ReplyMessageId == prompt.ReplyMessageId);
        RefreshPromptQueue();
    }

    private void RefreshPromptQueue()
    {
        if (PromptQueue is null) return;
        Guid? chatId = ActiveChatId;
        var messages = chatId is Guid id && _queuedPrompts.TryGetValue(id, out var queue) ? queue : [];
        PromptQueue.ShowMessages(messages, chatId is Guid active &&
            (HasQueuedTaskActivity(active) || messages.Any(prompt => prompt.IsTaskGuidance)));
        PresentUnknownOutcome();
    }

    private bool TryQueuePrompt()
    {
        if (_savingOnClose || _chatClosing || ActiveChatId is not Guid chatId
            || !HasQueuedTaskActivity(chatId)) return false;
        string text = PromptTextBox.Text.Trim();
        if (text.Length == 0 && !HasAttachmentDrafts) return true;
        // Sending during execution leaves an editable waiting card; only its explicit guidance action interrupts work.
        // 执行中发送先留下可编辑等待卡片，只有明确点击引导才补充当前任务。
        var prompt = new QueuedPrompt(Guid.NewGuid(), text, _selectedModel?.ProviderId,
            _selectedModel?.ModelId, _layout.PermissionMode, TakeAttachmentDrafts());
        QueueFor(chatId).Add(prompt);
        PromptTextBox.Text = ViewModel.Prompt = string.Empty;
        if (FindChat(chatId).Chat is { } chat) chat.Draft = string.Empty;
        RefreshPromptQueue();
        UpdateSendButtonState();
        PromptTextBox.Focus(FocusState.Programmatic);
        _ = DrainQueuedPromptsAsync(chatId);
        return true;
    }

    private async Task SteerQueuedPromptAsync(Guid chatId, QueuedPrompt prompt)
    {
        if (_chatClosing || _savingOnClose || !QueueFor(chatId).Contains(prompt) || prompt.IsSubmitting
            || !_steeringPromptChats.Add(chatId)) return;
        prompt.IsSubmitting = true;
        RefreshPromptQueue();
        try
        {
            if (!prompt.IsTaskGuidance)
            {
                if (!await AwaitQueueObservationAsync(chatId, retry: true))
                { ShowActionFeedback(UiText.Get("消息已保留，请重试。")); return; }
                if (_pendingReplies.TryGetValue(chatId, out var pending))
                {
                    prompt.TargetTaskId = pending.TaskId;
                    prompt.TargetRequestId = pending.StreamRequestId;
                }
                else if (_runningTaskRecovery is { } recovery && recovery.Chat.Id == chatId
                    && RunningTaskObservation.Select(QueueObservedTasks(recovery), chatId, FindChat(chatId).Project?.Id) is { } task
                    && Guid.TryParse(task.RequestId, out var requestId))
                { prompt.TargetTaskId = task.TaskId; prompt.TargetRequestId = requestId; }
                else
                {
                    if (HasQueuedTaskActivity(chatId)) ShowActionFeedback(UiText.Get("消息已保留，请重试。"));
                    return;
                }
                prompt.IsTaskGuidance = true;
            }
            await SendTaskSteeringAsync(chatId, prompt);
        }
        finally
        {
            prompt.IsSubmitting = false;
            _steeringPromptChats.Remove(chatId);
            RefreshPromptQueue();
            if (!prompt.IsTaskGuidance) _ = DrainQueuedPromptsAsync(chatId);
        }
    }

    private async Task DrainQueuedPromptsAsync(Guid chatId)
    {
        if (_chatClosing || _savingOnClose || _steeringPromptChats.Contains(chatId) || !_drainingPromptChats.Add(chatId)) return;
        try
        {
            var queue = QueueFor(chatId);
            if (!await AwaitQueueObservationAsync(chatId, retry: false)) return;
            if (_chatClosing || _savingOnClose || HasQueuedTaskActivity(chatId) || HasUnknownOutcome(chatId) || queue.Count == 0) return;
            var prompt = queue[0];
            if (prompt.IsSubmitting || prompt.IsTaskGuidance || FindChat(chatId).Chat is not { } chat) return;
            queue.Remove(prompt);
            if (!chat.Messages.Any(message => message.Id == prompt.Id))
                chat.Messages.Add(new() { Id = prompt.Id, Role = "user", Content = prompt.Text,
                    Attachments = prompt.Attachments.ToList() });
            if (prompt.AttachmentDrafts.Length > 0) _messageAttachmentDrafts[prompt.Id] = prompt.AttachmentDrafts;
            var ordinary = BeginPendingReply(chatId, prompt.Text, prompt.Provider, prompt.Model, prompt.PermissionMode,
                requestId: prompt.ReplyMessageId, userMessageId: prompt.Id);
            if (ActiveChatId == chatId) UpdateConversationPresentation(openAtBottom: false);
            _ = ReceiveReplyAsync(ordinary);
        }
        catch (OperationCanceledException) when (_historyLifetime.IsCancellationRequested || _chatClosing) { }
        finally
        {
            _drainingPromptChats.Remove(chatId);
            RefreshPromptQueue();
            UpdateSendButtonState();
        }
    }
}
