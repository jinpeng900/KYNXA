using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private sealed class PendingChatReply(Guid conversationId, string question, string? provider, string? model, string permissionMode)
    {
        public Guid ConversationId { get; } = conversationId;
        public string Question { get; } = question;
        public string? Provider { get; } = provider;
        public string? Model { get; } = model;
        public string PermissionMode { get; } = permissionMode;
        public CancellationTokenSource Cancellation { get; } = new();
        public string? Error { get; set; }
    }

    private readonly Dictionary<Guid, PendingChatReply> _pendingReplies = [];
    private bool _chatClosing;
    private Guid? ActiveChatId => (ViewModel.IsChatMode ? _activeStandaloneChat : _activeProjectChat)?.Id;

    private bool IsReplyInProgress(Guid? chatId) => chatId is Guid id &&
        _pendingReplies.TryGetValue(id, out var pending) && pending.Error is null;

    private void UpdateSendButtonState() => SendButton.IsEnabled = !_sendingPrompt && !IsReplyInProgress(ActiveChatId);

    private PendingChatReply BeginPendingReply(Guid chatId, string question, string? provider, string? model, string? permissionMode = null)
    {
        CancelPendingReply(chatId);
        var pending = new PendingChatReply(chatId, question, provider, model, permissionMode ?? _layout.PermissionMode);
        _pendingReplies.Add(chatId, pending);
        return pending;
    }

    private (ProjectChatState? Chat, ProjectState? Project) FindChat(Guid chatId)
    {
        var standalone = _standaloneChats.FirstOrDefault(chat => chat.Id == chatId);
        if (standalone is not null) return (standalone, null);
        foreach (var project in _projects)
        {
            var chat = project.Chats.FirstOrDefault(chat => chat.Id == chatId);
            if (chat is not null) return (chat, project);
        }
        return (null, null);
    }

    private async Task ReceiveMockReplyAsync(PendingChatReply pending)
    {
        try
        {
            var reply = await _modelApiClient.ReplyAsync(
                new ChatRequest(pending.ConversationId, pending.Question, pending.Model, pending.PermissionMode, pending.Provider), pending.Cancellation.Token);
            if (_chatClosing || !_pendingReplies.TryGetValue(pending.ConversationId, out var current) || current != pending) return;

            // Resolve by conversation ID after the request finishes; changing views must not redirect the reply.
            var (chat, project) = FindChat(pending.ConversationId);
            if (chat is null) { _pendingReplies.Remove(pending.ConversationId); return; }
            var message = new ChatMessageState { Id = reply.RequestId, Role = reply.Role, Content = reply.Content, CreatedAt = reply.CreatedAt };
            chat.Messages.Add(message);
            try
            {
                if (project is null) _projectStore.SaveChats(_standaloneChats);
                else _projectStore.Save(_projects);
            }
            catch
            {
                chat.Messages.Remove(message);
                throw;
            }
            _pendingReplies.Remove(pending.ConversationId);
        }
        catch (OperationCanceledException) when (_chatClosing || pending.Cancellation.IsCancellationRequested) { }
        catch (Exception error) when (error is System.Net.Http.HttpRequestException or OperationCanceledException or
            System.Text.Json.JsonException or IOException or UnauthorizedAccessException or InvalidOperationException)
        {
            pending.Error = error is IOException or UnauthorizedAccessException
                ? "回复未能保存，请重试。" : error is InvalidOperationException
                ? error.Message : "暂时没有收到回复，请确认模型服务已启动后重试。";
        }
        finally
        {
            if (!_chatClosing && ActiveChatId == pending.ConversationId) UpdateConversationPresentation();
            // Keep failed requests for the retry button; completed requests no longer need a token source.
            if (!_pendingReplies.TryGetValue(pending.ConversationId, out var current) || current != pending)
                pending.Cancellation.Dispose();
        }
    }

    private async void RetryReply_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: Guid chatId } || !_pendingReplies.TryGetValue(chatId, out var failed) || failed.Error is null) return;
        if (FindChat(chatId).Chat is null) return;
        var pending = BeginPendingReply(chatId, failed.Question, failed.Provider, failed.Model, failed.PermissionMode);
        if (ActiveChatId == chatId) UpdateConversationPresentation();
        await ReceiveMockReplyAsync(pending);
    }

    private void CancelPendingReply(Guid chatId)
    {
        if (!_pendingReplies.Remove(chatId, out var pending)) return;
        pending.Cancellation.Cancel();
        pending.Cancellation.Dispose();
    }

    private void StopMockReplies()
    {
        _chatClosing = true;
        foreach (Guid chatId in _pendingReplies.Keys.ToArray()) CancelPendingReply(chatId);
        _modelApiClient.Dispose();
    }
}
