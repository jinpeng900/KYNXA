using CommunityToolkit.Mvvm.ComponentModel;
using KYNXA_Desktop.Models.UI;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.ViewModels;

public sealed class ConversationMessageViewModel(Guid conversationId, ChatMessageState message) : ObservableObject
{
    public Guid ConversationId { get; } = conversationId;
    public ChatMessageState Message { get; } = message;
    private bool _isThinking;
    private bool _isReasoningExpanded;
    private bool _canRetry;
    public string Content => Message.Content;
    public string ReasoningText => _isReasoningExpanded ? Message.Reasoning : string.Empty;
    public bool IsStreaming => Message.Status == "streaming";
    public bool IsThinking => IsStreaming && _isThinking;
    public bool IsWaiting => IsStreaming && Content.Length == 0 && Message.Reasoning.Length == 0;
    public string ErrorText => Message.Status == "interrupted" ? "已停止生成" + (Message.Error.Length > 0 ? " · " + Message.Error : "") : Message.Error;
    public string ReasoningTitle => IsThinking ? "正在思考…" : $"思考过程 · {Math.Max(1, (long)Math.Ceiling(Message.ReasoningDurationMs / 1000d))} 秒";
    public string ReasoningGlyph => _isReasoningExpanded ? "\uE70D" : "\uE76C";
    public Visibility UserVisibility => Message.Role == "user" ? Visibility.Visible : Visibility.Collapsed;
    public Visibility AssistantVisibility => Message.Role == "user" ? Visibility.Collapsed : Visibility.Visible;
    public Visibility ReplyVisibility => Content.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
    public Visibility WaitingVisibility => IsWaiting ? Visibility.Visible : Visibility.Collapsed;
    public Visibility ThinkingVisibility => IsThinking ? Visibility.Visible : Visibility.Collapsed;
    public Visibility ReasoningVisibility => Message.Reasoning.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
    public Visibility ReasoningBodyVisibility => _isReasoningExpanded ? Visibility.Visible : Visibility.Collapsed;
    public Visibility ErrorVisibility => Message.Status is "error" or "interrupted" || Message.Error.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
    public Visibility RetryVisibility => _canRetry && Message.Status is "error" or "interrupted" ? Visibility.Visible : Visibility.Collapsed;

    public void SetRetryAllowed(bool allowed)
    {
        _canRetry = allowed;
        OnPropertyChanged(nameof(RetryVisibility));
    }

    public void Refresh(bool isThinking = false)
    {
        _isThinking = isThinking;
        // Notify the current message only; leave other reply controls and their selections intact.
        OnPropertyChanged(string.Empty);
    }

    public void ToggleReasoning()
    {
        _isReasoningExpanded = !_isReasoningExpanded;
        OnPropertyChanged(nameof(ReasoningText));
        OnPropertyChanged(nameof(ReasoningGlyph));
        OnPropertyChanged(nameof(ReasoningBodyVisibility));
    }
}
