using KYNXA_Desktop.Services;
using KYNXA_Desktop.Models.UI;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private readonly Dictionary<Guid, List<QueuedPrompt>> _queuedPrompts = [];
    private readonly HashSet<Guid> _drainingPromptChats = [];
    private readonly HashSet<Guid> _steeringPromptChats = [];

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
    }

    private List<QueuedPrompt> QueueFor(Guid chatId)
    {
        if (!_queuedPrompts.TryGetValue(chatId, out var queue)) _queuedPrompts[chatId] = queue = [];
        return queue;
    }

    private void RefreshPromptQueue()
    {
        if (PromptQueue is null) return;
        Guid? chatId = ActiveChatId;
        var messages = chatId is Guid id && _queuedPrompts.TryGetValue(id, out var queue) ? queue : [];
        PromptQueue.ShowMessages(messages, chatId is Guid active
            && (IsReplyInProgress(active) || _pausedTaskIds.ContainsKey(active)));
    }

    private bool TryQueuePrompt()
    {
        if (_savingOnClose || _chatClosing || ActiveChatId is not Guid chatId
            || !(IsReplyInProgress(chatId) || _drainingPromptChats.Contains(chatId) || _pausedTaskIds.ContainsKey(chatId)))
            return false;
        string text = PromptTextBox.Text.Trim();
        if (text.Length == 0 && !HasAttachmentDrafts) return true;
        // Queue entries become formal user messages only when sent or explicitly steered.
        // 排队内容在执行或明确引导时才成为正式用户消息，编辑等待项不会修改聊天日志。
        QueueFor(chatId).Add(new(Guid.NewGuid(), text, _selectedModel?.ProviderId,
            _selectedModel?.ModelId, _layout.PermissionMode, TakeAttachmentDrafts()));
        PromptTextBox.Text = ViewModel.Prompt = string.Empty;
        if (FindChat(chatId).Chat is { } chat) chat.Draft = string.Empty;
        RefreshPromptQueue();
        UpdateSendButtonState();
        PromptTextBox.Focus(FocusState.Programmatic);
        return true;
    }

    private async Task SteerQueuedPromptAsync(Guid chatId, QueuedPrompt prompt)
    {
        if (_chatClosing || prompt.IsSubmitting || !_steeringPromptChats.Add(chatId)) return;
        prompt.IsSubmitting = true;
        QueueFor(chatId).Remove(prompt);
        RefreshPromptQueue();
        try
        {
            if (!await SendTaskSteeringAsync(chatId, prompt)) QueueFor(chatId).Insert(0, prompt);
        }
        finally
        {
            prompt.IsSubmitting = false;
            _steeringPromptChats.Remove(chatId);
            RefreshPromptQueue();
            if (!IsReplyInProgress(chatId) && !_pausedTaskIds.ContainsKey(chatId))
                _ = DrainQueuedPromptsAsync(chatId);
        }
    }

    private async Task DrainQueuedPromptsAsync(Guid chatId)
    {
        if (_chatClosing || _savingOnClose || _pausedTaskIds.ContainsKey(chatId)
            || IsReplyInProgress(chatId) || _steeringPromptChats.Contains(chatId) || !_drainingPromptChats.Add(chatId)) return;
        try
        {
            var queue = QueueFor(chatId);
            while (!_chatClosing && !_savingOnClose && !_pausedTaskIds.ContainsKey(chatId)
                && !IsReplyInProgress(chatId) && !_steeringPromptChats.Contains(chatId) && queue.Count > 0)
            {
                var prompt = queue[0];
                if (prompt.IsSubmitting || FindChat(chatId).Chat is not { } chat) break;
                queue.RemoveAt(0);
                chat.Messages.Add(new ChatMessageState { Id = prompt.Id, Role = "user", Content = prompt.Text,
                    Attachments = prompt.Attachments.ToList() });
                if (prompt.AttachmentDrafts.Length > 0) _messageAttachmentDrafts[prompt.Id] = prompt.AttachmentDrafts;
                var pending = BeginPendingReply(chatId, prompt.Text, prompt.Provider, prompt.Model, prompt.PermissionMode);
                RefreshPromptQueue();
                if (ActiveChatId == chatId) UpdateConversationPresentation(openAtBottom: false);
                await ReceiveReplyAsync(pending);
                // A paused or failed request leaves the remaining queue untouched.
                // 暂停或失败时保留尚未执行的队列，不以自动请求掩盖当前任务的停止。
                if (pending.Message.Status != "completed") break;
            }
        }
        finally
        {
            _drainingPromptChats.Remove(chatId);
            RefreshPromptQueue();
            UpdateSendButtonState();
        }
    }
}
