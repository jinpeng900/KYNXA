using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private readonly Dictionary<Guid, ApprovalPresentation> _approvalPresentations = [];
    private Guid? _visibleApprovalId;
    private long _approvalSequence;

    private sealed class ApprovalPresentation(PendingChatReply pending, ToolActivity tool, Guid? taskId, long sequence)
    {
        public PendingChatReply Pending { get; } = pending;
        public ToolActivity Tool { get; } = tool;
        public Guid? TaskId { get; } = taskId;
        public long Sequence { get; } = sequence;
        public CancellationTokenRegistration CancellationRegistration { get; set; }
        public bool IsSubmitting { get; set; }
        public string? Error { get; set; }
    }

    private bool IsCurrentApproval(PendingChatReply pending) => !_chatClosing && !pending.Cancellation.IsCancellationRequested &&
        ActiveChatId == pending.ConversationId && _pendingReplies.TryGetValue(pending.ConversationId, out var current) && current == pending;

    private void NotifyApprovalConversationChanged() => RefreshInlineApproval();

    private void QueueToolApproval(PendingChatReply pending, ToolActivity tool, Guid? taskId = null)
    {
        if (tool.ApprovalId is not Guid approvalId || approvalId == Guid.Empty || pending.Cancellation.IsCancellationRequested ||
            _approvalPresentations.ContainsKey(approvalId)) return;
        var presentation = new ApprovalPresentation(pending, tool, taskId, ++_approvalSequence);
        _approvalPresentations.Add(approvalId, presentation);
        // Cancellation only dismisses this request; navigation and hiding never supply a decision.
        // 取消只移除此请求；导航及隐藏卡片均不替用户作出审批决定。
        presentation.CancellationRegistration = pending.Cancellation.Token.Register(() => DispatcherQueue.TryEnqueue(() =>
        {
            RemoveInlineApproval(approvalId, presentation);
            RefreshInlineApproval();
        }));
        RefreshInlineApproval();
    }

    private void RefreshInlineApproval()
    {
        foreach (var item in _approvalPresentations.ToArray())
            if (item.Value.Pending.Cancellation.IsCancellationRequested)
                RemoveInlineApproval(item.Key, item.Value);
        // Dictionary slots can be reused after another chat's approval closes; preserve explicit arrival order.
        // 其他聊天的审批关闭后字典槽位会被重用，展示顺序必须使用明确的到达序号。
        var eligible = _approvalPresentations.Where(item => IsCurrentApproval(item.Value.Pending))
            .OrderBy(item => item.Value.Sequence).ToArray();
        if (!IsLoaded || XamlRoot is null || eligible.Length == 0)
        {
            HideInlineApproval();
            return;
        }
        var selected = eligible.FirstOrDefault(item => item.Key == _visibleApprovalId);
        if (selected.Value is null) selected = eligible[0];
        _visibleApprovalId = selected.Key;
        InlineApprovalCard.Configure(selected.Value.Tool, Array.FindIndex(eligible, item => item.Key == selected.Key) + 1, eligible.Length);
        InlineApprovalCard.SetSubmitting(selected.Value.IsSubmitting);
        if (selected.Value.Error is { } error) InlineApprovalCard.ShowError(error);
        ShowInlineApproval();
    }

    private async void InlineApprovalDecisionRequested(object? sender, ToolApprovalChoice choice)
    {
        if (_visibleApprovalId is not Guid approvalId || !_approvalPresentations.TryGetValue(approvalId, out var presentation) ||
            presentation.IsSubmitting || !IsCurrentApproval(presentation.Pending)) return;
        presentation.IsSubmitting = true;
        presentation.Error = null;
        RefreshInlineApproval();
        var cancellation = presentation.Pending.Cancellation.Token;
        try
        {
            await _agentApiClient.SubmitApprovalAsync(new(presentation.Pending.ConversationId, presentation.Pending.Message.Id,
                presentation.Tool.ToolCallId, approvalId, choice != ToolApprovalChoice.Reject,
                choice == ToolApprovalChoice.Task ? "task" : "once"), cancellation);
            // Retire only the acknowledged identity. The gateway closes other task-granted requests through stream events.
            // 仅移除已确认的审批身份；同一任务获批的其他请求由网关事件逐条关闭。
            RemoveInlineApproval(approvalId, presentation);
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested) { RemoveInlineApproval(approvalId, presentation); }
        catch (GatewayApiException error) when (error.ErrorCode is "APPROVAL_NOT_FOUND" or "TOOL_APPROVAL_EXPIRED" or "TOOL_RUN_TIMEOUT" or "TASK_DEADLINE_EXCEEDED")
        {
            // An authoritative expired/closed identity cannot be retried; transport failures remain recoverable below.
            // 网关明确确认过期或关闭的身份不能重试；传输失败则仍由下方保留重试入口。
            RemoveInlineApproval(approvalId, presentation);
        }
        catch (Exception error) when (error is System.Net.Http.HttpRequestException or InvalidOperationException or
            InvalidDataException or System.Text.Json.JsonException or OperationCanceledException)
        {
            if (_approvalPresentations.TryGetValue(approvalId, out var current) && current == presentation)
            {
                presentation.IsSubmitting = false;
                presentation.Error = UiText.Get("审批提交失败，请重试。");
            }
        }
        finally { RefreshInlineApproval(); }
    }

    private void RemoveInlineApproval(Guid approvalId, ApprovalPresentation presentation)
    {
        if (!_approvalPresentations.TryGetValue(approvalId, out var current) || current != presentation) return;
        _approvalPresentations.Remove(approvalId);
        presentation.CancellationRegistration.Dispose();
    }

    private void CloseTaskApproval(Guid approvalId, Guid? taskId)
    {
        if (_approvalPresentations.TryGetValue(approvalId, out var presentation) && presentation.TaskId == taskId)
            RemoveInlineApproval(approvalId, presentation);
        RefreshInlineApproval();
    }

    private void CloseReplyApprovals(PendingChatReply pending)
    {
        foreach (var item in _approvalPresentations.Where(item => item.Value.Pending == pending).ToArray())
            RemoveInlineApproval(item.Key, item.Value);
        RefreshInlineApproval();
    }

    private void AcceptToolActivity(PendingChatReply pending, ToolActivity tool)
    {
        if (tool.Status != "approval-required")
        {
            foreach (var item in _approvalPresentations.Where(item => item.Value.Pending == pending &&
                item.Value.TaskId is null && item.Value.Tool.ToolCallId == tool.ToolCallId).ToArray())
                RemoveInlineApproval(item.Key, item.Value);
            RefreshInlineApproval();
        }
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
