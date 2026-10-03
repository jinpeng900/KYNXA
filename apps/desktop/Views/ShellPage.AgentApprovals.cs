using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private bool IsCurrentApproval(PendingChatReply pending) => !_chatClosing && !pending.Cancellation.IsCancellationRequested &&
        ActiveChatId == pending.ConversationId && _pendingReplies.TryGetValue(pending.ConversationId, out var current) && current == pending;

    private async Task RequestToolApprovalAsync(PendingChatReply pending, ToolActivity tool)
    {
        if (tool.ApprovalId is not { } approvalId) throw new InvalidDataException(UiText.Get("工具审批缺少请求标识。"));
        bool approved = false;
        if (IsCurrentApproval(pending) && XamlRoot is not null)
        {
            var dialog = ToolApprovalDialog.Create(XamlRoot, tool);
            using var cancellation = pending.Cancellation.Token.Register(() => DispatcherQueue.TryEnqueue(dialog.Hide));
            try { approved = await dialog.ShowAsync() == ContentDialogResult.Primary && IsCurrentApproval(pending); }
            catch (InvalidOperationException) { approved = false; }
        }
        // Stream cancellation revokes the pending server operation; it cannot be approved by a late dialog result.
        if (_chatClosing || pending.Cancellation.IsCancellationRequested) return;
        await _agentApiClient.SubmitApprovalAsync(new(pending.ConversationId, pending.Message.Id, tool.ToolCallId, approvalId, approved),
            pending.Cancellation.Token);
    }

    private static void AcceptToolActivity(PendingChatReply pending, ToolActivity tool)
    {
        var activities = pending.Message.ToolActivities;
        int index = activities.FindIndex(item => item.ToolCallId == tool.ToolCallId);
        if (index < 0) activities.Add(tool);
        else activities[index] = tool;
        pending.Dirty = true;
    }
}
