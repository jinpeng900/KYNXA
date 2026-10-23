using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Views;

/// <summary>
/// Overlay an unknown-outcome notice without changing queued messages.
/// 行内提示当前聊天的未知结果，仅覆盖排队区，不删除、发送或重排消息。
/// </summary>
public sealed partial class ShellPage
{
    private sealed record UnknownOutcomeState(TaskRecord Task, TaskOperation[] Operations, TaskModelReservation[] Reservations);
    private readonly Dictionary<Guid, UnknownOutcomeState> _unknownOutcomes = [];
    private readonly Dictionary<Guid, Task> _unknownOutcomeReads = [];
    private readonly HashSet<Guid> _unknownOutcomeControls = [];
    private Guid? _unknownOutcomeCheckedChat;
    private bool _unknownOutcomeReplyActive;

    private void InitializeUnknownOutcomeCard()
    {
        UnknownOutcomeCard.ConfirmRequested += (_, request) => _ = ResolveUnknownOutcomeAsync(request, confirm: true);
        UnknownOutcomeCard.CancelRequested += (_, request) => _ = ResolveUnknownOutcomeAsync(request, confirm: false);
        // Task panel reads already observe state changes; reuse them instead of a separate timer.
        // 任务面板的读取已能观察状态变化；复用其通知，不另设计时器。
        TaskPanel.TasksChanged += (_, _) =>
        {
            if (ActiveChatId is Guid chatId) _ = RefreshUnknownOutcomeAsync(chatId);
        };
    }

    private bool HasUnknownOutcome(Guid chatId) => _unknownOutcomes.ContainsKey(chatId);

    /// <summary>Called after the queue renders; only visibility changes, never the queue contents.
    /// 排队区渲染后调用；只改变显示，不改变排队内容。</summary>
    private void PresentUnknownOutcome()
    {
        if (UnknownOutcomeCard is null) return;
        Guid? chatId = ActiveChatId;
        bool replyActive = chatId is Guid current && IsReplyInProgress(current);
        if (chatId != _unknownOutcomeCheckedChat || (_unknownOutcomeReplyActive && !replyActive))
        {
            // Re-read when the chat changes or its reply ends; a running task cannot be waiting for verification.
            // 切换聊天或回复结束时重新读取；正在运行的任务不会处于等待核验。
            _unknownOutcomeCheckedChat = chatId;
            if (chatId is Guid id) _ = RefreshUnknownOutcomeAsync(id);
        }
        _unknownOutcomeReplyActive = replyActive;
        if (chatId is Guid active && _unknownOutcomes.TryGetValue(active, out var state))
        {
            UnknownOutcomeCard.Show(state.Task, state.Operations, state.Reservations);
            UnknownOutcomeCard.SetQueuedCount(_queuedPrompts.TryGetValue(active, out var queue) ? queue.Count : 0);
            PromptQueue.Visibility = Visibility.Collapsed;
        }
        else UnknownOutcomeCard.Hide();
    }

    private Task RefreshUnknownOutcomeAsync(Guid chatId)
    {
        if (_chatClosing) return Task.CompletedTask;
        if (_unknownOutcomeReads.TryGetValue(chatId, out var running)) return running;
        var read = ReadUnknownOutcomeAsync(chatId);
        if (!read.IsCompleted) _unknownOutcomeReads[chatId] = read;
        return read;
    }

    private async Task ReadUnknownOutcomeAsync(Guid chatId)
    {
        try
        {
            var (chat, project) = FindChat(chatId);
            if (chat is null || !chat.IsPersisted)
            {
                _unknownOutcomes.Remove(chatId);
                return;
            }
            using var deadline = CancellationTokenSource.CreateLinkedTokenSource(_historyLifetime.Token);
            deadline.CancelAfter(TimeSpan.FromSeconds(30));
            var list = await _tasksApiClient.RecoverableAsync(chatId, project?.Id, null, deadline.Token);
            UnknownOutcomeState? found = null;
            foreach (var task in list.Tasks.Where(task => task.State == "waiting-verification")
                .OrderByDescending(task => task.UpdatedAtMs))
            {
                var detail = await _tasksApiClient.GetAsync(chatId, task.TaskId, project?.Id, deadline.Token);
                var operations = (detail.RecoveryOperations ?? detail.Operations).Where(TaskOperationStates.NeedsUnknownAcknowledgement).ToArray();
                var reservations = (detail.RecoveryModelReservations ?? []).Where(item => item.State == "unknown").ToArray();
                if (detail.Task.State == "waiting-verification" && (operations.Length > 0 || reservations.Length > 0))
                {
                    found = new(detail.Task, operations, reservations);
                    break;
                }
            }
            if (_chatClosing || FindChat(chatId).Chat != chat) return;
            if (found is not null) _unknownOutcomes[chatId] = found;
            else _unknownOutcomes.Remove(chatId);
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException
            or InvalidOperationException or InvalidDataException or System.Text.Json.JsonException or ArgumentException)
        {
            // Keep the last confirmed state; a failed read never hides or invents a pending decision.
            // 读取失败保留上次确认的状态，不隐藏也不虚构待处理事项。
            System.Diagnostics.Debug.WriteLine($"Unknown outcome read: {error.GetType().Name}");
        }
        finally
        {
            _unknownOutcomeReads.Remove(chatId);
            if (!_chatClosing && ActiveChatId == chatId) RefreshPromptQueue();
        }
    }

    private async Task ResolveUnknownOutcomeAsync(UnknownOutcomeRequest request, bool confirm)
    {
        Guid chatId = request.Task.Scope.ConversationId;
        if (_chatClosing || ActiveChatId != chatId || !_unknownOutcomeControls.Add(chatId)) return;
        UnknownOutcomeCard.SetBusy(true);
        UnknownOutcomeCard.ShowFeedback(null);
        string? feedback = null;
        try
        {
            if (confirm) await ResumeSelectedTaskAsync(request.Task, request.OperationIds, request.ModelReservationIds, request.Operations);
            else feedback = await CancelUnknownOutcomeTaskAsync(request.Task);
        }
        finally
        {
            _unknownOutcomeControls.Remove(chatId);
            await RefreshUnknownOutcomeAsync(chatId);
            if (!_chatClosing)
            {
                UnknownOutcomeCard.SetBusy(false);
                // Still waiting after an explicit control means it was not confirmed; say so inside the card.
                // 明确操作后仍在等待说明未获确认，只在卡片内说明，不弹窗。
                if (UnknownOutcomeCard.TaskId == request.Task.TaskId && ActiveChatId == chatId)
                    UnknownOutcomeCard.ShowFeedback(feedback ?? "任务控制未确认，排队消息已保留，请稍后重试。");
            }
        }
    }

    /// <summary>Resume flow hook: drop the notice as soon as the gateway reports the task left verification.
    /// 继续流程回调：网关确认任务离开等待核验后立即移除提示，不等待整段回复结束。</summary>
    private void OnUnknownOutcomeControlled(Guid chatId, TaskRecord task)
    {
        if (task.State == "waiting-verification" || !_unknownOutcomes.TryGetValue(chatId, out var state)
            || state.Task.TaskId != task.TaskId) return;
        _unknownOutcomes.Remove(chatId);
        RefreshPromptQueue();
    }

    private async Task<string?> CancelUnknownOutcomeTaskAsync(TaskRecord task)
    {
        Guid chatId = task.Scope.ConversationId;
        try
        {
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
            var detail = await _tasksApiClient.GetAsync(chatId, task.TaskId, task.Scope.ProjectId, deadline.Token);
            if (!TaskStates.IsTerminal(detail.Task.State))
                await _tasksApiClient.ControlAsync(chatId, task.TaskId, detail.Task.Version, "cancel",
                    task.Scope.ProjectId, deadline.Token);
            _unknownOutcomes.Remove(chatId);
            TaskPanel.NotifyChanged(chatId);
            return null;
        }
        catch (GatewayApiException error)
        {
            return error.Message;
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException
            or InvalidOperationException or InvalidDataException or System.Text.Json.JsonException or ArgumentException)
        {
            return "任务控制未确认，排队消息已保留，请稍后重试。";
        }
    }
}
