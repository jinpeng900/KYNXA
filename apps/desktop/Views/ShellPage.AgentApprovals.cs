using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
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
        // Inactive conversations retain their pending decision without occupying the inline surface.
        // 非当前聊天保留待审批状态，只等待聊天激活事件，不占用输入区上方的审批位置。
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
            ToolApprovalDecision? decision = null;
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
            // A resumed stream can have a different reply anchor; approval belongs to its transport request.
            // 恢复流可能使用不同回复锚点；审批必须绑定当前传输请求，不能改绑展示消息。
            var response = await _agentApiClient.SubmitApprovalAsync(new(presentation.Pending.ConversationId,
                presentation.Pending.StreamRequestId, tool.ToolCallId, approvalId, decision.Approved, decision.Scope), cancellation);
            if (decision.Scope == "task" && response.Scope != "task" && IsCurrentApproval(presentation.Pending))
                ShowActionFeedback(UiText.Get("本次操作已批准，当前网关未建立任务授权。"));
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
        if (!_approvalPresentations.TryGetValue(approvalId, out var presentation)) return;
        Guid? expectedTask = presentation.TaskId ?? presentation.Pending.TaskId;
        if (taskId is null || expectedTask is null || expectedTask == taskId) presentation.Cancellation.Cancel();
    }

    private void CloseReplyApprovals(PendingChatReply pending)
    {
        foreach (var presentation in _approvalPresentations.Values.Where(item => item.Pending == pending).ToArray())
            presentation.Cancellation.Cancel();
    }

    private async Task<ToolApprovalDecision?> RequestToolApprovalAsync(PendingChatReply pending, ToolActivity tool,
        CancellationToken cancellationToken)
    {
        if (tool.ApprovalId is null) throw new InvalidDataException(UiText.Get("工具审批缺少请求标识。"));
        if (!IsCurrentApproval(pending) || XamlRoot is null) return null;
        var source = new TaskCompletionSource<ToolApprovalDecision>(TaskCreationOptions.RunContinuationsAsynchronously);
        var card = new InlineToolApprovalCard(tool);
        void Decide(ToolApprovalDecision decision) => source.TrySetResult(decision);
        var root = XamlRoot;
        void Resize(XamlRoot sender, XamlRootChangedEventArgs args) => card.SetAvailableHeight(sender.Size.Height);
        card.DecisionSelected += Decide;
        root.Changed += Resize;
        card.SetAvailableHeight(root.Size.Height);
        InlineApprovalHost.Children.Add(card);
        InlineApprovalHost.Visibility = Visibility.Visible;
        using var cancellation = cancellationToken.Register(() => source.TrySetCanceled(cancellationToken));
        try
        {
            var decision = await source.Task;
            // Changing conversation or closing the card is not an approval or a refusal.
            // 切换聊天或关闭展示不等于批准或拒绝；正式任务决定仍只来自用户按钮。
            return _chatClosing || cancellationToken.IsCancellationRequested || !IsCurrentApproval(pending) ? null : decision;
        }
        finally
        {
            root.Changed -= Resize;
            card.DecisionSelected -= Decide;
            InlineApprovalHost.Children.Remove(card);
            if (InlineApprovalHost.Children.Count == 0) InlineApprovalHost.Visibility = Visibility.Collapsed;
        }
    }

    private void AcceptToolActivity(PendingChatReply pending, ToolActivity tool)
    {
        var activities = pending.Message.ToolActivities;
        int index = activities.FindIndex(item => item.ToolCallId == tool.ToolCallId);
        if (index < 0) activities.Add(tool);
        else activities[index] = tool;
        pending.Dirty = true;
        // Present live display receipts before the stream can finish and lose its pending marker.
        // 实时展示回执先交给右栏，不等待流结束后才从历史中推断是否应该打开。
        QueueWorkspaceResourceDiscovery(pending.ConversationId, pending.Message.Id, tool, liveReply: true);
    }
}
