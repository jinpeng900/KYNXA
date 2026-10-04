using KYNXA_Desktop.Services;
using System.Diagnostics;
using System.Text;
using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private sealed class PendingChatReply(Guid conversationId, Guid userMessageId, string question, string? provider, string? model, string permissionMode)
    {
        public Guid ConversationId { get; } = conversationId;
        public Guid UserMessageId { get; } = userMessageId;
        public string Question { get; } = question;
        public string? Provider { get; } = provider;
        public string? Model { get; } = model;
        public string PermissionMode { get; } = permissionMode;
        public CancellationTokenSource Cancellation { get; } = new();
        public ChatMessageState Message { get; } = new() { Role = "assistant", Status = "streaming", Provider = provider ?? "", Model = model ?? "" };
        public ConversationMessageViewModel Presentation { get; set; } = null!;
        public StringBuilder Content { get; } = new();
        public StringBuilder Reasoning { get; } = new();
        public Dictionary<string, (StringBuilder Content, StringBuilder Reasoning)> SegmentText { get; } = new(StringComparer.Ordinal);
        public HashSet<string> DirtySegments { get; } = new(StringComparer.Ordinal);
        public Stopwatch ThinkingTime { get; } = new();
        public bool Dirty { get; set; }
        public string? Error => Message.Status == "streaming" ? null : Message.Error;
    }

    private readonly Dictionary<Guid, PendingChatReply> _pendingReplies = [];
    private DispatcherTimer? _replyRefreshTimer;
    private bool _chatClosing;
    private Guid? ActiveChatId => (ViewModel.IsChatMode ? _activeStandaloneChat : _activeProjectChat)?.Id;

    private bool IsReplyInProgress(Guid? chatId) => chatId is Guid id && _pendingReplies.ContainsKey(id);

    private void UpdateSendButtonState()
    {
        bool active = ActiveChatId is Guid id && _pendingReplies.ContainsKey(id);
        SendButton.IsEnabled = active || !_sendingPrompt;
        SendArrow.Visibility = active ? Visibility.Collapsed : Visibility.Visible;
        StopReplyIcon.Visibility = active ? Visibility.Visible : Visibility.Collapsed;
        AutomationProperties.SetName(SendButton, active ? UiText.Get("停止生成") : UiText.Get("发送"));
        ToolTipService.SetToolTip(SendButton, active ? UiText.Get("停止生成") : UiText.Get("发送"));
    }

    private PendingChatReply BeginPendingReply(Guid chatId, string question, string? provider, string? model, string? permissionMode = null, Guid? requestId = null)
    {
        var (chat, _) = FindChat(chatId);
        if (chat is null || _pendingReplies.ContainsKey(chatId)) throw new InvalidOperationException(UiText.Get("当前聊天正在生成回复。"));
        var user = chat.Messages.LastOrDefault(message => message.Role == "user")
            ?? throw new InvalidOperationException(UiText.Get("找不到要回复的用户消息。"));
        var pending = new PendingChatReply(chatId, user.Id, question, provider, model, permissionMode ?? _layout.PermissionMode);
        if (requestId is Guid existingId) pending.Message.Id = existingId;
        pending.Presentation = new ConversationMessageViewModel(chatId, pending.Message);
        pending.Presentation.SetRetryAllowed(true);
        chat.Messages.Add(pending.Message);
        _pendingReplies.Add(chatId, pending);
        RenderProjects();
        if (_replyRefreshTimer is null)
        {
            _replyRefreshTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(40) };
            _replyRefreshTimer.Tick += (_, _) =>
            {
                foreach (var reply in _pendingReplies.Values.ToArray()) FlushReply(reply);
            };
        }
        _replyRefreshTimer.Start();
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

    private void FlushReply(PendingChatReply pending, bool final = false)
    {
        if (!pending.Dirty && !final) return;
        foreach (string id in pending.DirtySegments)
        {
            int index = pending.Message.AssistantSegments.FindIndex(segment => segment.Id == id);
            if (index >= 0 && pending.SegmentText.TryGetValue(id, out var text))
                pending.Message.AssistantSegments[index] = pending.Message.AssistantSegments[index] with
                { Content = text.Content.ToString(), Reasoning = text.Reasoning.ToString() };
        }
        pending.DirtySegments.Clear();
        pending.Message.Content = pending.Content.ToString();
        pending.Message.Reasoning = pending.Reasoning.ToString();
        pending.Message.ReasoningDurationMs = pending.ThinkingTime.ElapsedMilliseconds;
        pending.Presentation.Refresh(pending.ThinkingTime.IsRunning);
        pending.Dirty = false;
    }

    private async Task ReceiveReplyAsync(PendingChatReply pending)
    {
        try
        {
            await foreach (var update in _modelApiClient.StreamReplyAsync(
                new ChatRequest(pending.ConversationId, pending.Question, pending.Model, pending.PermissionMode, pending.Provider, pending.Message.Id, pending.UserMessageId), pending.Cancellation.Token))
            {
                if (_chatClosing || !_pendingReplies.TryGetValue(pending.ConversationId, out var current) || current != pending) return;
                switch (update.Type)
                {
                    case "tool_call":
                    case "tool_result":
                        if (update.Tool is { } activity)
                        {
                            AcceptToolActivity(pending, activity);
                        }
                        break;
                    case "terminal_output":
                        continue; // Command output stays in the formal tool receipt; it has no sidebar projection.
                        // 命令输出保留在正式工具回执中，不生成侧栏展示投影。
                    case "approval_required":
                        if (update.Tool is { } approval)
                        {
                            AcceptToolActivity(pending, approval);
                            FlushReply(pending);
                            await RequestToolApprovalAsync(pending, approval);
                        }
                        break;
                    case "reasoning_delta":
                        pending.ThinkingTime.Start();
                        pending.Reasoning.Append(update.Delta);
                        AppendSegmentDelta(pending, update, reasoning: true);
                        break;
                    case "text_delta":
                        pending.ThinkingTime.Stop();
                        pending.Content.Append(update.Delta);
                        AppendSegmentDelta(pending, update, reasoning: false);
                        break;
                    case "assistant_segment":
                        if (update.Segment is { } segment) AcceptAssistantSegment(pending, segment);
                        break;
                    case "content_snapshot":
                        pending.Content.Clear();
                        pending.Content.Append(update.Content);
                        pending.Reasoning.Clear();
                        pending.Reasoning.Append(update.Reasoning);
                        break;
                    case "completed":
                    case "interrupted":
                    case "error":
                        pending.ThinkingTime.Stop();
                        if (update.Content is not null) { pending.Content.Clear(); pending.Content.Append(update.Content); }
                        if (update.Reasoning is not null) { pending.Reasoning.Clear(); pending.Reasoning.Append(update.Reasoning); }
                        if (update.AssistantSegments is { } segments)
                        {
                            pending.Message.AssistantSegments.Clear();
                            pending.Message.AssistantSegments.AddRange(segments);
                            pending.SegmentText.Clear();
                            pending.DirtySegments.Clear();
                        }
                        pending.Message.Status = update.Type == "completed" ? "completed" : update.Type;
                        pending.Message.Error = update.Error ?? string.Empty;
                        pending.Message.DurationMs = update.DurationMs;
                        break;
                }
                pending.Dirty = true;
            }
            if (pending.Message.Status == "streaming")
            {
                pending.Message.Status = "interrupted";
                pending.Message.Error = UiText.Get("连接已结束，回复尚未完成。");
            }
        }
        catch (OperationCanceledException) when (pending.Cancellation.IsCancellationRequested || _chatClosing)
        { pending.Message.Status = "interrupted"; pending.Message.Error = string.Empty; }
        catch (Exception error) when (error is System.Net.Http.HttpRequestException or OperationCanceledException or
            System.Text.Json.JsonException or IOException or UnauthorizedAccessException or InvalidOperationException)
        {
            pending.Message.Status = "error";
            pending.Message.Error = error is InvalidOperationException or InvalidDataException ? error.Message :
                UiText.Get("连接中断，已保留收到的内容。请检查模型服务后重试。");
        }
        finally
        {
            pending.ThinkingTime.Stop();
            if (pending.Message.Status != "completed")
            {
                var segments = pending.Message.AssistantSegments;
                for (int index = 0; index < segments.Count; index++)
                    if (segments[index].Status == "streaming") segments[index] = segments[index] with { Status = "interrupted" };
            }
            if (_pendingReplies.TryGetValue(pending.ConversationId, out var current) && current == pending)
            {
                FlushReply(pending, final: true);
                _pendingReplies.Remove(pending.ConversationId);
            }
            pending.Cancellation.Dispose();
            if (_pendingReplies.Count == 0) _replyRefreshTimer?.Stop();
            if (!_chatClosing)
            {
                UpdateSendButtonState();
                RenderProjects();
            }
        }
    }

    private static void AcceptAssistantSegment(PendingChatReply pending, AssistantSegment segment)
    {
        var segments = pending.Message.AssistantSegments;
        int index = segments.FindIndex(value => value.Id == segment.Id);
        if (index < 0) segments.Add(segment);
        else segments[index] = segment;
        pending.SegmentText[segment.Id] = (new StringBuilder(segment.Content), new StringBuilder(segment.Reasoning));
        pending.DirtySegments.Remove(segment.Id);
        if (segment.Status != "streaming") pending.ThinkingTime.Stop();
        pending.Dirty = true;
    }

    private static void AppendSegmentDelta(PendingChatReply pending, ChatStreamEvent update, bool reasoning)
    {
        if (update.SegmentId is null) return;
        if (!pending.SegmentText.TryGetValue(update.SegmentId, out var text)) return;
        (reasoning ? text.Reasoning : text.Content).Append(update.Delta);
        pending.DirtySegments.Add(update.SegmentId);
    }

    private async void Transcript_RetryRequested(object? sender, Guid messageId)
    {
        var failed = ActiveMessages.FirstOrDefault(row => row.Message.Id == messageId);
        if (failed is null || failed.ToolActivities.Count > 0 || IsReplyInProgress(failed.ConversationId)) return;
        var (chat, _) = FindChat(failed.ConversationId);
        if (chat is null || chat.Messages.LastOrDefault() != failed.Message) return;
        string? question = chat.Messages.Take(chat.Messages.Count - 1).LastOrDefault(message => message.Role == "user")?.Content;
        if (question is null) return;
        // Replace only the failed attempt; never add a second copy of the user's question.
        // 只替换失败的尝试，不重复添加用户问题。
        chat.Messages.Remove(failed.Message);
        var pending = BeginPendingReply(chat.Id, question,
            string.IsNullOrEmpty(failed.Message.Provider) ? _selectedModel?.ProviderId : failed.Message.Provider,
            string.IsNullOrEmpty(failed.Message.Model) ? _selectedModel?.ModelId : failed.Message.Model, requestId: failed.Message.Id);
        if (ActiveChatId == chat.Id)
        {
            int index = ActiveMessages.IndexOf(failed);
            if (index >= 0)
            {
                ActiveMessages[index] = pending.Presentation;
                ConversationMessages.ShowConversation(chat.Id, ActiveMessages.ToArray(), openAtBottom: false);
                ScreenshotPanel.ShowConversation(chat.Id, ActiveMessages.ToArray());
            }
            else UpdateConversationPresentation();
        }
        UpdateSendButtonState();
        await ReceiveReplyAsync(pending);
    }

    private void CancelPendingReply(Guid chatId)
    {
        if (!_pendingReplies.Remove(chatId, out var pending)) return;
        pending.ThinkingTime.Stop();
        pending.Message.Content = pending.Content.ToString();
        pending.Message.Reasoning = pending.Reasoning.ToString();
        pending.Message.ReasoningDurationMs = pending.ThinkingTime.ElapsedMilliseconds;
        pending.Message.Status = "interrupted";
        pending.Presentation.Refresh();
        pending.Cancellation.Cancel();
        RenderProjects();
    }

    private void StopReplies()
    {
        _chatClosing = true;
        CancelModelPickerRefresh();
        _replyRefreshTimer?.Stop();
        foreach (var pending in _pendingReplies.Values.ToArray())
        {
            pending.Cancellation.Cancel();
            pending.ThinkingTime.Stop();
            pending.Message.Status = "interrupted";
            FlushReply(pending, final: true);
        }
        _modelApiClient.Dispose();
        _toolResultCancellation?.Cancel();
        ScreenshotPanel.Dispose();
        WorkTabs.Dispose();
        MountedWorkspace.Dispose();
        _agentApiClient.Dispose();
        ConversationMessages.Dispose();
    }
}
