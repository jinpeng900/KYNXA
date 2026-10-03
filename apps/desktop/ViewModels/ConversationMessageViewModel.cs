using KYNXA_Desktop.Models.UI;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.ViewModels;

public sealed class ConversationMessageViewModel(Guid conversationId, ChatMessageState? message = null, string? error = null)
{
    public Guid ConversationId { get; } = conversationId;
    public Guid? MessageId => message?.Id;
    public string Content => message?.Content ?? string.Empty;
    public string ErrorText => error ?? string.Empty;
    public bool IsWaiting => message is null && error is null;
    public Visibility UserVisibility => message?.Role == "user" ? Visibility.Visible : Visibility.Collapsed;
    public Visibility AssistantVisibility => message?.Role == "user" ? Visibility.Collapsed : Visibility.Visible;
    public Visibility ReplyVisibility => message is not null ? Visibility.Visible : Visibility.Collapsed;
    public Visibility WaitingVisibility => IsWaiting ? Visibility.Visible : Visibility.Collapsed;
    public Visibility ErrorVisibility => error is not null ? Visibility.Visible : Visibility.Collapsed;
}
