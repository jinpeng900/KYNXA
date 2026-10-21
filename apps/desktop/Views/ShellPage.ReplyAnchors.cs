using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private ChatMessageState PrepareSteeringReply(Guid conversation, QueuedPrompt prompt, PendingChatReply? pending)
    {
        var chat = FindChat(conversation).Chat!;
        var previous = pending?.Message ?? chat.Messages.LastOrDefault(message => message.Role == "assistant"
            && message.SupersededByUserMessageId is null && message.Status != "completed");
        if (pending is not null) FlushReply(pending, final: true);
        if (previous is not null)
        {
            previous.Status = "interrupted";
            previous.Error = string.Empty;
            previous.SupersededByUserMessageId = prompt.Id;
        }
        var submitted = chat.Messages.FirstOrDefault(message => message.Id == prompt.Id)
            ?? new ChatMessageState { Id = prompt.Id, Role = "user", Content = prompt.Text,
                Attachments = prompt.Attachments.ToList() };
        if (prompt.AttachmentDrafts.Length > 0) _messageAttachmentDrafts[prompt.Id] = prompt.AttachmentDrafts;
        if (!chat.Messages.Contains(submitted)) chat.Messages.Add(submitted);
        var reply = new ChatMessageState
        {
            Id = prompt.ReplyMessageId, Role = "assistant", Status = "streaming",
            Provider = previous?.Provider ?? prompt.Provider ?? "", Model = previous?.Model ?? prompt.Model ?? "",
            InReplyToUserMessageId = prompt.Id,
            OriginUserMessageId = pending?.UserMessageId ?? previous?.OriginUserMessageId
        };
        chat.Messages.Add(reply);
        if (pending is not null)
        {
            SetPendingReplyMessage(pending, reply);
            pending.WaitingReplyAnchorId = reply.Id;
        }
        if (ActiveChatId == conversation) UpdateConversationPresentation(openAtBottom: false);
        return submitted;
    }

    private static void SetPendingReplyMessage(PendingChatReply pending, ChatMessageState message)
    {
        // Guidance replaces the reply row, not the running task or its existing monotonic clock.
        // 插话仅更换回复行；同一任务沿用已有单调计时，等待新输出期间也不能隐藏或归零。
        if (pending.Message.GenerationStartedTimestamp is long taskStarted &&
            (message.GenerationStartedTimestamp is not long replyStarted || taskStarted < replyStarted))
            message.GenerationStartedTimestamp = taskStarted;
        pending.Message = message;
        pending.Content.Clear(); pending.Content.Append(message.Content);
        pending.Reasoning.Clear(); pending.Reasoning.Append(message.Reasoning);
        pending.SegmentText.Clear(); pending.DirtySegments.Clear();
        foreach (var segment in message.AssistantSegments)
            pending.SegmentText[segment.Id] = (new(segment.Content), new(segment.Reasoning));
        pending.Presentation = new ConversationMessageViewModel(pending.ConversationId, message);
        pending.Dirty = true;
    }

    private void AcceptReplyAnchor(PendingChatReply pending, ChatStreamEvent update)
    {
        if (update.ReplyMessageId is not Guid replyId || update.UserMessageId is not Guid userId
            || FindChat(pending.ConversationId).Chat is not { } chat) return;
        // A later accepted steer owns new output; earlier output remains above it without further appends.
        // 后来的引导接收新输出，先前输出保留在上方且不再追加。
        if (pending.WaitingReplyAnchorId is Guid waiting && waiting != replyId) return;
        if (update.SupersededReplyMessageId is Guid oldId
            && chat.Messages.FirstOrDefault(message => message.Id == oldId) is { } old)
        {
            old.Status = "interrupted";
            old.SupersededByUserMessageId = userId;
        }
        if (pending.Message.Id != replyId)
        {
            var reply = chat.Messages.FirstOrDefault(message => message.Id == replyId)
                ?? new ChatMessageState { Id = replyId, Role = "assistant", Status = "streaming",
                    Provider = pending.Provider ?? "", Model = pending.Model ?? "",
                    InReplyToUserMessageId = userId, OriginUserMessageId = pending.UserMessageId };
            if (!chat.Messages.Contains(reply)) chat.Messages.Add(reply);
            SetPendingReplyMessage(pending, reply);
        }
        pending.WaitingReplyAnchorId = null;
        if (ActiveChatId == chat.Id) UpdateConversationPresentation(openAtBottom: false);
    }
}
