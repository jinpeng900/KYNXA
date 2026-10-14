using KYNXA_Desktop.Services;
using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.ViewModels;
using KYNXA_Desktop.Controls;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private readonly TasksApiClient _tasksApiClient = new();
    private readonly Dictionary<Guid, Guid> _pausedTaskIds = [];

    private void InitializeTaskPanel()
    {
        TaskPanel.ConfigureApi(_tasksApiClient);
        TaskPanel.ControlApplied += TaskPanel_ControlApplied;
        Unloaded += (_, _) => TaskPanel.SetPreviewEnabled(false);
    }

    private void SynchronizeTaskPanel(bool sidebarVisible)
    {
        NotifyApprovalConversationChanged();
        Guid? projectId = ActiveChatId is Guid chat ? FindChat(chat).Project?.Id : null;
        TaskPanel.ShowConversation(ActiveChatId, projectId);
        TaskPanel.SetPreviewEnabled(sidebarVisible && !_chatClosing);
    }

    private async Task StopPendingReplyAsync(PendingChatReply pending)
    {
        pending.StopRequested = true;
        if (pending.StopControlPending || _chatClosing) return;
        pending.StopControlPending = true;
        bool waitingRegistration = false;
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(15));
        try
        {
            if (pending.TaskId is not Guid taskId)
            {
                waitingRegistration = true;
                return;
            }
            var (_, project) = FindChat(pending.ConversationId);
            var detail = await _tasksApiClient.GetAsync(pending.ConversationId, taskId, project?.Id, deadline.Token);
            if (TaskStates.IsTerminal(detail.Task.State)) return;
            var receipt = await _tasksApiClient.ControlAsync(pending.ConversationId, taskId, detail.Task.Version,
                "pause", project?.Id, deadline.Token);
            pending.PauseConfirmed = receipt.Task.State == "paused";
            if (!pending.PauseConfirmed) throw new InvalidDataException(UiText.Get("任务暂停结果尚未确认，请查看任务状态。"));
            _pausedTaskIds[pending.ConversationId] = taskId;
            if (pending.Message.Status != "streaming")
            {
                pending.Message.Error = UiText.Get("已暂停，进度已保存。");
                FlushReply(pending, final: true);
            }
            if (!_chatClosing && _pendingReplies.TryGetValue(pending.ConversationId, out var current) && current == pending)
                pending.Cancellation.Cancel();
            TaskPanel.NotifyChanged(pending.ConversationId);
        }
        catch (GatewayApiException error) when (error.StatusCode == System.Net.HttpStatusCode.NotFound)
        {
            // Registration and stream acceptance can race; pause once the durable task identity arrives.
            // 正式任务登记与流接受可能并发；收到持久任务身份后再暂停，不能替换为取消。
            waitingRegistration = true;
        }
        catch (Exception error) when (error is System.Net.Http.HttpRequestException or OperationCanceledException
            or InvalidOperationException or InvalidDataException or System.Text.Json.JsonException)
        {
            if (!_chatClosing) ShowActionFeedback(UiText.Get("任务暂停结果尚未确认，请查看任务状态。"));
            TaskPanel.NotifyChanged(pending.ConversationId);
        }
        finally
        {
            pending.StopControlPending = false;
            if (waitingRegistration && pending.Started && pending.StopRegistrationRetries++ < 1 && !_chatClosing)
                _ = StopPendingReplyAsync(pending);
            else if (waitingRegistration && pending.Started && !_chatClosing)
                ShowActionFeedback(UiText.Get("任务暂停结果尚未确认，请查看任务状态。"));
        }
    }

    private async void TaskPanel_ControlApplied(object? sender, TaskControlResponse receipt)
    {
        if (_chatClosing || receipt.Task.ParentTaskId is not null) return;
        if (receipt.Task.State == "paused")
        {
            _pausedTaskIds[receipt.Task.Scope.ConversationId] = receipt.Task.TaskId;
            return;
        }
        if (receipt.Task.State is "queued" or "running") await ObserveResumedTaskAsync(receipt.Task);
        else if (TaskStates.IsTerminal(receipt.Task.State)) _pausedTaskIds.Remove(receipt.Task.Scope.ConversationId);
    }

    private async Task<bool> TrySteerPausedTaskAsync()
    {
        if (ActiveChatId is not Guid conversation || string.IsNullOrWhiteSpace(PromptTextBox.Text)) return false;
        if (_pausedTaskIds.TryGetValue(conversation, out var taskId)) return await SendTaskSteeringAsync(taskId);
        var (chat, owner) = FindChat(conversation);
        // An empty new draft has no formal chat or task yet; persisted chats remain eligible even before history loads.
        // 空的新聊天草稿尚无正式聊天及任务；已持久化的旧聊天即使历史未加载也必须查询。
        if (chat is null || !chat.CanPersist) return false;
        string draft = PromptTextBox.Text;
        Guid? project = owner?.Id;
        Guid? recoveredTaskId = null;
        // Own the send gate during recovery; a double click or a changed chat must not start another request.
        // 恢复查询独占发送入口；双击或切换聊天不能创建另一请求，也不扫描消息日志。
        _sendingPrompt = true;
        UpdateSendButtonState();
        try
        {
            var response = await _tasksApiClient.PausedAsync(conversation, project);
            if (_chatClosing || _savingOnClose || ActiveChatId != conversation || PromptTextBox.Text != draft
                || FindChat(conversation).Project?.Id != project) return true;
            var selected = PausedTaskSelection.From(response);
            if (selected.IsAmbiguous)
            {
                ShowActionFeedback(UiText.Get("此聊天有多个暂停任务，请在任务侧栏选择要继续的任务；输入内容已保留。"));
                return true;
            }
            recoveredTaskId = selected.TaskId;
            if (recoveredTaskId is Guid paused) _pausedTaskIds[conversation] = paused;
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException
            or InvalidOperationException or InvalidDataException or System.Text.Json.JsonException or ArgumentException)
        {
            if (!_chatClosing && ActiveChatId == conversation)
                ShowActionFeedback(UiText.Get("暂停任务读取失败，输入内容已保留；请重试或在任务侧栏查看。"));
            return true;
        }
        finally { _sendingPrompt = false; UpdateSendButtonState(); }
        return recoveredTaskId is Guid recovered && await SendTaskSteeringAsync(recovered);
    }

    private async Task<bool> SendTaskSteeringAsync(Guid? pausedTaskId = null)
    {
        if (_sendingPrompt || _savingOnClose || _chatClosing || ActiveChatId is not Guid conversation) return false;
        string draft = PromptTextBox.Text;
        string text = draft.Trim();
        if (text.Length == 0) return false;
        _pendingReplies.TryGetValue(conversation, out var pending);
        Guid? taskId = pausedTaskId ?? pending?.TaskId;
        if (taskId is null)
        {
            ShowActionFeedback(UiText.Get("任务正在准备，补充内容已保留，请稍后发送。"));
            return true;
        }
        _sendingPrompt = true;
        UpdateSendButtonState();
        ChatMessageState? submitted = null;
        try
        {
            var actions = new ScrollSafeComboBox { HorizontalAlignment = Microsoft.UI.Xaml.HorizontalAlignment.Stretch, SelectedIndex = 0 };
            actions.Items.Add(UiText.Get("补充并继续当前任务"));
            actions.Items.Add(UiText.Get("按补充内容更新方案"));
            actions.Items.Add(UiText.Get("取消当前任务"));
            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot, Title = UiText.Get("调整当前任务"), Content = actions,
                PrimaryButtonText = UiText.Get("确认"), CloseButtonText = UiText.Get("返回"),
                DefaultButton = ContentDialogButton.Primary
            };
            if (await dialog.ShowAsync() != ContentDialogResult.Primary || _chatClosing || ActiveChatId != conversation) return true;
            var (chat, project) = FindChat(conversation);
            if (chat is null) return true;
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(60));
            var current = await _tasksApiClient.GetAsync(conversation, taskId.Value, project?.Id, deadline.Token);
            if (TaskStates.IsTerminal(current.Task.State))
            {
                _pausedTaskIds.Remove(conversation);
                ShowActionFeedback(UiText.Get("原任务已经结束，输入内容已保留。"));
                return true;
            }
            if (actions.SelectedIndex == 2)
            {
                await _tasksApiClient.ControlAsync(conversation, taskId.Value, current.Task.Version, "cancel", project?.Id, deadline.Token);
                _pausedTaskIds.Remove(conversation);
                TaskPanel.NotifyChanged(conversation);
                return true;
            }
            // Render the update before I/O; the gateway saves its formal message and binds steering to its source/hash.
            // 先展示用户补充再进行通信；网关保存正式消息，并以来源与哈希绑定任务插话。
            submitted = new ChatMessageState { Role = "user", Content = text };
            chat.Messages.Add(submitted);
            if (PromptTextBox.Text == draft) PromptTextBox.Text = ViewModel.Prompt = string.Empty;
            UpdateConversationPresentation(openAtBottom: false);
            var receipt = await _tasksApiClient.SteerAsync(conversation, taskId.Value, current.Task.Version,
                submitted.Id, text, actions.SelectedIndex == 1 ? "update" : "continue", project?.Id, deadline.Token);
            _pausedTaskIds.Remove(conversation);
            TaskPanel.NotifyChanged(conversation);
            if (receipt.Task.State == "paused")
            {
                _pausedTaskIds[conversation] = taskId.Value;
                if (pending is not null) { pending.PauseConfirmed = true; pending.Cancellation.Cancel(); }
                ShowActionFeedback(UiText.Get("补充已保存，正在等待旧操作结束。"));
                return true;
            }
            if (receipt.ReconnectRequired && pending is not null)
            {
                // The gateway has fenced the old attempt; close only its observation and attach to the resumed task.
                // 网关已停止旧尝试的后续派发；这里只关闭观察流，再接回同一个任务。
                pending.PauseConfirmed = true;
                pending.Cancellation.Cancel();
            }
            if (pending is null || receipt.ReconnectRequired) await ObserveResumedTaskAsync(receipt.Task);
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException
            or InvalidOperationException or InvalidDataException or System.Text.Json.JsonException or ArgumentException)
        {
            if (!_chatClosing)
            {
                if (submitted is not null && (error is ArgumentException
                    || error is GatewayApiException gateway && (int)gateway.StatusCode < 500))
                {
                    FindChat(conversation).Chat?.Messages.Remove(submitted);
                    if (string.IsNullOrWhiteSpace(PromptTextBox.Text)) PromptTextBox.Text = ViewModel.Prompt = draft;
                    UpdateConversationPresentation(openAtBottom: false);
                }
                ShowActionFeedback(UiText.Get("任务补充未确认，原任务进度已保留，请查看状态。"));
                TaskPanel.NotifyChanged(conversation);
            }
        }
        finally { _sendingPrompt = false; UpdateSendButtonState(); }
        return true;
    }

    private async Task ObserveResumedTaskAsync(TaskRecord task)
    {
        if (_chatClosing || task.ParentTaskId is not null || task.State is not ("queued" or "running")
            || !Guid.TryParse(task.RequestId, out var requestId)) return;
        if (_pendingReplies.TryGetValue(task.Scope.ConversationId, out var prior))
        {
            if (prior.TaskId != task.TaskId || !prior.PauseConfirmed) return;
            try { await prior.Finished.Task.WaitAsync(TimeSpan.FromSeconds(15)); }
            catch (TimeoutException)
            {
                if (!_chatClosing) ShowActionFeedback(UiText.Get("任务已继续，打开原聊天可查看回复。"));
                return;
            }
        }
        var (chat, _) = FindChat(task.Scope.ConversationId);
        var message = chat?.Messages.FirstOrDefault(item => item.Id == requestId && item.Role == "assistant");
        if (chat is null || message is null)
        {
            if (!_chatClosing) ShowActionFeedback(UiText.Get("任务已继续，打开原聊天可查看回复。"));
            return;
        }
        int position = chat.Messages.IndexOf(message);
        var original = chat.Messages.Take(position).LastOrDefault(item => item.Role == "user");
        if (original is null) return;
        try
        {
            await MessageTimePresentation.ForgetAsync(chat.Id, message);
            if (_chatClosing || _pendingReplies.ContainsKey(chat.Id)) return;
            var pending = new PendingChatReply(chat.Id, original.Id, original.Content, message.Provider, message.Model,
                _layout.PermissionMode, message) { TaskId = task.TaskId, Started = true };
            message.Status = "streaming"; message.Error = string.Empty;
            pending.Content.Append(message.Content); pending.Reasoning.Append(message.Reasoning);
            foreach (var segment in message.AssistantSegments)
                pending.SegmentText[segment.Id] = (new System.Text.StringBuilder(segment.Content), new System.Text.StringBuilder(segment.Reasoning));
            pending.Presentation = new ConversationMessageViewModel(chat.Id, message);
            _pendingReplies.Add(chat.Id, pending);
            _pausedTaskIds.Remove(chat.Id);
            StartReplyRefreshTimer();
            if (ActiveChatId == chat.Id) UpdateConversationPresentation(openAtBottom: false);
            UpdateSendButtonState();
            _ = ReceiveReplyAsync(pending, task.TaskId);
        }
        catch (Exception error) when (error is InvalidOperationException or IOException or UnauthorizedAccessException)
        { if (!_chatClosing) ShowActionFeedback(UiText.Get("任务已继续，打开原聊天可查看回复。")); }
    }
}
