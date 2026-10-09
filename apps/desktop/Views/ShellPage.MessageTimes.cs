using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private readonly HashSet<Guid> _retryingReplies = [];

    private async Task SaveReplyTimeAsync(Guid conversationId, ChatMessageState message)
    {
        if (!await MessageTimePresentation.SaveAsync(conversationId, message) && !_chatClosing)
            ShowActionFeedback(UiText.Get("回复时间未能保存到本机缓存，聊天内容不受影响。"));
    }

    private async Task RestoreDeletedChatTimesAsync(ProjectChatState chat)
    {
        foreach (var message in chat.Messages.ToArray()) await SaveReplyTimeAsync(chat.Id, message);
    }
}
