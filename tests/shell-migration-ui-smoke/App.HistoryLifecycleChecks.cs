using System.Collections;
using System.Reflection;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop;

public partial class App
{
    private async Task CheckHistoryLifecycleAsync(ProjectChatState chat, object pending)
    {
        var pages = ShellField<IDictionary>("_historyPages");
        var replies = ShellField<IDictionary>("_pendingReplies");
        var originalState = pages[chat.Id] ?? throw new InvalidOperationException("Selected history must be initialized.");
        var historyType = originalState.GetType();
        await WaitAsync(() => !(bool)historyType.GetProperty("Loading")!.GetValue(originalState)!,
            "the selected history finishes its ordinary refresh before lifecycle checks");
        var originalMessages = chat.Messages.ToList();
        var message = (ChatMessageState)pending.GetType().GetProperty("Message")!.GetValue(pending)!;
        string originalContent = message.Content, originalStatus = message.Status;
        var recent = _shell.ViewModel.RecentConversations.Single(item => item.Id == chat.Id);
        TaskCompletionSource? responseGate = null;

        object ResetState()
        {
            var state = Activator.CreateInstance(historyType, BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic,
                null, [chat], null)!;
            historyType.GetProperty("Initialized")!.SetValue(state, true);
            pages[chat.Id] = state;
            return state;
        }
        ConversationHistoryPage Page(string text, string status) => new(chat.Id,
            originalMessages.Select(item => item.Id == message.Id
                ? new ChatMessageState { Id = item.Id, Role = "assistant", Content = text, Status = status, CreatedAt = item.CreatedAt }
                : item).ToList(), originalMessages.Count, false, null, Guid.NewGuid().ToString("D"), 0, false);
        Task StartPage(object state, ConversationHistoryPage page, bool fail = false)
        {
            _gateway.HistoryPageOverride = page;
            _gateway.FailNextHistoryPage = fail;
            _gateway.HistoryPageEntered = new(TaskCreationOptions.RunContinuationsAsynchronously);
            responseGate = new(TaskCreationOptions.RunContinuationsAsynchronously);
            _gateway.HistoryPageGate = responseGate;
            return (Task)Call("LoadHistoryAsync", state, "latest")!;
        }
        try
        {
            // Keep the actual local pending reference even when it settles before the old page returns.
            // 在途旧页返回前本机回复可能已结算并移出活动表，仍须保留原消息对象和最终内容。
            var loading = StartPage(ResetState(), Page("STALE_INFLIGHT_BODY", "streaming"));
            await _gateway.HistoryPageEntered!.Task;
            message.Content = "LOCAL_COMPLETED_DURING_HISTORY";
            message.Status = "completed";
            replies.Remove(chat.Id);
            responseGate!.TrySetResult();
            await loading;
            Check(chat.Messages.Single(item => item.Id == message.Id) == message
                && message.Content == "LOCAL_COMPLETED_DURING_HISTORY" && message.Status == "completed",
                "a delayed history page cannot roll back a reply that completed and left the pending table while it was in flight");

            chat.Messages = originalMessages.ToList();
            message.Content = "STALE_FORMAL_STREAMING"; message.Status = "streaming";
            loading = StartPage(ResetState(), Page("FORMAL_COMPLETED_WITHOUT_LOCAL_PENDING", "completed"));
            await _gateway.HistoryPageEntered!.Task;
            responseGate!.TrySetResult();
            await loading;
            var settled = chat.Messages.Single(item => item.Id == message.Id);
            Check(settled.Status == "completed" && settled.Content == "FORMAL_COMPLETED_WITHOUT_LOCAL_PENDING"
                && !ReferenceEquals(settled, message),
                "a stale historical streaming flag without a real local pending reply yields to the formal completed page");

            replies[chat.Id] = pending;
            chat.Messages = originalMessages.ToList();
            message.Content = originalContent; message.Status = originalStatus;
            loading = StartPage(ResetState(), Page("OBSOLETE_HISTORY_MUST_NOT_APPLY", "completed"), fail: true);
            await _gateway.HistoryPageEntered!.Task;
            Call("SetPrimaryMode", false, true);
            string feedback = Element<TextBlock>("ActionFeedbackText").Text;
            responseGate!.TrySetResult();
            await loading;
            Check(ActiveId != chat.Id && Element<TextBlock>("ActionFeedbackText").Text == feedback
                && NativeUi.OpenDialog(_shell) is null && _unhandled is null,
                "a background history failure cannot replace another conversation's feedback or open a global error dialog");
        }
        finally
        {
            responseGate?.TrySetResult();
            _gateway.HistoryPageOverride = null; _gateway.HistoryPageEntered = null;
            _gateway.HistoryPageGate = null; _gateway.FailNextHistoryPage = false;
            message.Content = originalContent; message.Status = originalStatus;
            chat.Messages = originalMessages;
            replies[chat.Id] = pending;
            pages[chat.Id] = originalState;
            Call("SetPrimaryMode", true, true);
            Call("SelectStandaloneChat", recent);
        }
    }
}
