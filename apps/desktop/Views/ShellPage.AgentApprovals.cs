using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private readonly SemaphoreSlim _approvalSurface = new(1, 1);
    private readonly Dictionary<Guid, ApprovalPresentation> _approvalPresentations = [];
    private TaskCompletionSource<bool> _approvalConversationChanged = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private Guid? _approvalPresentedConversationId;
    private bool _approvalRootAvailable;
    private sealed record ApprovalPresentation(PendingChatReply Pending, Guid? TaskId, CancellationTokenSource Cancellation)
    {
        public CancellationTokenSource? DisplayCancellation { get; set; }
    }

    private bool IsCurrentApproval(PendingChatReply pending) => !_chatClosing && !pending.Cancellation.IsCancellationRequested &&
        ActiveChatId == pending.ConversationId && _pendingReplies.TryGetValue(pending.ConversationId, out var current) && current == pending;

    private void NotifyApprovalConversationChanged()
    {
        bool rootAvailable = XamlRoot is not null;
        if (_approvalPresentedConversationId == ActiveChatId && _approvalRootAvailable == rootAvailable) return;
        _approvalPresentedConversationId = ActiveChatId;
        _approvalRootAvailable = rootAvailable;
        var previous = _approvalConversationChanged;
        _approvalConversationChanged = new(TaskCreationOptions.RunContinuationsAsynchronously);
        foreach (var presentation in _approvalPresentations.Values.ToArray())
            if (!IsCurrentApproval(presentation.Pending) || !rootAvailable) presentation.DisplayCancellation?.Cancel();
        previous.TrySetResult(true);
    }

    private async Task WaitForApprovalConversationAsync(ApprovalPresentation presentation)
    {
        // Inactive conversations retain their pending decision without holding the global dialog surface.
        // 非当前聊天保留待审批状态，只等待聊天激活事件，不占用全局弹窗位置。
        while (!IsCurrentApproval(presentation.Pending) || XamlRoot is null)
            await _approvalConversationChanged.Task.WaitAsync(presentation.Cancellation.Token);
    }

    private void QueueToolApproval(PendingChatReply pending, ToolActivity tool, Guid? taskId = null)
    {
        if (tool.ApprovalId is not Guid approvalId || _approvalPresentations.ContainsKey(approvalId)) return;
        var presentation = new ApprovalPresentation(pending, taskId,
            CancellationTokenSource.CreateLinkedTokenSource(pending.Cancellation.Token));
        _approvalPresentations.Add(approvalId, presentation);
        _ = PresentQueuedApprovalAsync(approvalId, tool, presentation);
    }

    private async Task PresentQueuedApprovalAsync(Guid approvalId, ToolActivity tool, ApprovalPresentation presentation)
    {
        var cancellation = presentation.Cancellation.Token;
        try
        {
            bool? decision = null;
            while (decision is null)
            {
                await WaitForApprovalConversationAsync(presentation);
                var changed = _approvalConversationChanged.Task;
                using var display = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
                presentation.DisplayCancellation = display;
                bool acquired = false;
                try
                {
                    await _approvalSurface.WaitAsync(display.Token); acquired = true;
                    if (IsCurrentApproval(presentation.Pending))
                        decision = await RequestToolApprovalAsync(presentation.Pending, tool, display.Token);
                }
                catch (OperationCanceledException) when (display.IsCancellationRequested && !cancellation.IsCancellationRequested) { }
                finally
                {
                    if (acquired) _approvalSurface.Release();
                    presentation.DisplayCancellation = null;
                }
                if (decision is null && IsCurrentApproval(presentation.Pending)) await changed.WaitAsync(cancellation);
            }
            cancellation.ThrowIfCancellationRequested();
            // The user's explicit decision is sent once, after releasing the presentation semaphore.
            // 用户明确作出的决定只提交一次；提交期间不占弹窗位置，切换聊天不重放请求。
            await _agentApiClient.SubmitApprovalAsync(new(presentation.Pending.ConversationId, presentation.Pending.Message.Id,
                tool.ToolCallId, approvalId, decision.Value), cancellation);
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested) { }
        catch (Exception error) when (error is System.Net.Http.HttpRequestException or InvalidOperationException
            or InvalidDataException or System.Text.Json.JsonException or OperationCanceledException)
        {
            if (!_chatClosing && !cancellation.IsCancellationRequested)
                ShowActionFeedback(UiText.Get("工具审批未确认，请查看任务状态。"));
        }
        finally
        {
            if (_approvalPresentations.TryGetValue(approvalId, out var current) && current == presentation)
                _approvalPresentations.Remove(approvalId);
            presentation.Cancellation.Dispose();
        }
    }

    private void CloseTaskApproval(Guid approvalId, Guid? taskId)
    {
        if (_approvalPresentations.TryGetValue(approvalId, out var presentation) && presentation.TaskId == taskId)
            presentation.Cancellation.Cancel();
    }

    private void CloseReplyApprovals(PendingChatReply pending)
    {
        foreach (var presentation in _approvalPresentations.Values.Where(item => item.Pending == pending).ToArray())
            presentation.Cancellation.Cancel();
    }

    private async Task<bool?> RequestToolApprovalAsync(PendingChatReply pending, ToolActivity tool, CancellationToken cancellationToken)
    {
        if (tool.ApprovalId is null) throw new InvalidDataException(UiText.Get("工具审批缺少请求标识。"));
        if (!IsCurrentApproval(pending) || XamlRoot is null) return null;
        // A new approval takes the dialog surface from a history preview, without deciding for the user.
        // 新的审批可以占用历史预览的对话框，但不能替用户作出决定。
        if (_toolResultClosed is { } preview)
        {
            _toolResultCancellation?.Cancel();
            await preview.Task.WaitAsync(cancellationToken);
        }
        if (!IsCurrentApproval(pending) || XamlRoot is null || cancellationToken.IsCancellationRequested) return null;
        bool? decision = null;
        var dialog = ToolApprovalDialog.Create(XamlRoot, tool);
        dialog.PrimaryButtonClick += (_, _) => decision = true;
        dialog.CloseButtonClick += (_, _) => decision = false;
        using var cancellation = cancellationToken.Register(() => DispatcherQueue.TryEnqueue(() => dialog.Hide()));
        try { await dialog.ShowAsync(); }
        catch (InvalidOperationException) { return null; }
        // A closed presentation cannot grant permission; actual task cancellation belongs to the gateway control API.
        // 已关闭的展示不能授予权限；正式任务取消属于网关控制 API。
        if (_chatClosing || cancellationToken.IsCancellationRequested || !IsCurrentApproval(pending)) return null;
        return decision;
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
