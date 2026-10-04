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
        // A new approval takes the dialog surface from a history preview, without deciding for the user.
        // 新的审批可以占用历史预览的对话框，但不能替用户作出决定。
        if (_toolResultClosed is { } preview)
        {
            _toolResultCancellation?.Cancel();
            await preview.Task;
        }
        if (IsCurrentApproval(pending) && XamlRoot is not null)
        {
            var dialog = ToolApprovalDialog.Create(XamlRoot, tool);
            using var cancellation = pending.Cancellation.Token.Register(() => DispatcherQueue.TryEnqueue(() => dialog.Hide()));
            try { approved = await dialog.ShowAsync() == ContentDialogResult.Primary && IsCurrentApproval(pending); }
            catch (InvalidOperationException) { approved = false; }
        }
        // Stream cancellation revokes the pending server operation; it cannot be approved by a late dialog result.
        // 取消流会撤销待执行的服务器操作；晚到的对话框结果不能批准它。
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
