using CommunityToolkit.Mvvm.ComponentModel;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using KYNXA.Contracts;

namespace KYNXA_Desktop.ViewModels;

public sealed class ConversationMessageViewModel(Guid conversationId, ChatMessageState message) : ObservableObject
{
    public Guid ConversationId { get; } = conversationId;
    public ChatMessageState Message { get; } = message;
    private bool _isThinking;
    private bool _isReasoningExpanded;
    private bool _canRetry;
    public string Content => Message.Content;
    public IReadOnlyList<ToolActivity> ToolActivities => Message.ToolActivities;
    public string ReasoningText => _isReasoningExpanded ? Message.Reasoning : string.Empty;
    public bool IsStreaming => Message.Status == "streaming";
    public bool IsThinking => IsStreaming && _isThinking;
    public bool IsWaiting => IsStreaming && Content.Length == 0 && Message.Reasoning.Length == 0 && ToolActivities.Count == 0;
    public long ReasoningSeconds => Math.Max(1, (long)Math.Ceiling(Message.ReasoningDurationMs / 1000d));
    public string ErrorText => Message.Status == "interrupted" ? UiText.Get("已停止生成") + (Message.Error.Length > 0 ? " · " + Message.Error : "") : Message.Error;
    public string ReasoningTitle => IsThinking ? UiText.Get("正在思考…") : string.Format(UiText.Get("思考过程 · {0} 秒"), ReasoningSeconds);
    public string ReasoningGlyph => _isReasoningExpanded ? "\uE70D" : "\uE76C";
    public Visibility UserVisibility => Message.Role == "user" ? Visibility.Visible : Visibility.Collapsed;
    public Visibility AssistantVisibility => Message.Role == "user" ? Visibility.Collapsed : Visibility.Visible;
    public Visibility ReplyVisibility => Content.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
    public Visibility WaitingVisibility => IsWaiting ? Visibility.Visible : Visibility.Collapsed;
    public Visibility ThinkingVisibility => IsThinking ? Visibility.Visible : Visibility.Collapsed;
    public Visibility ReasoningVisibility => Message.Reasoning.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
    public Visibility ReasoningBodyVisibility => _isReasoningExpanded ? Visibility.Visible : Visibility.Collapsed;
    public Visibility ErrorVisibility => Message.Status is "error" or "interrupted" || Message.Error.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
    public Visibility RetryVisibility => _canRetry && ToolActivities.Count == 0 && Message.Status is "error" or "interrupted" ? Visibility.Visible : Visibility.Collapsed;

    public void SetRetryAllowed(bool allowed)
    {
        _canRetry = allowed;
        OnPropertyChanged(nameof(RetryVisibility));
    }

    public void Refresh(bool isThinking = false)
    {
        _isThinking = isThinking;
        // Notify the current message only; leave other reply controls and their selections intact.
        // 仅通知当前消息更新；保留其他回复控件及其选择。
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
