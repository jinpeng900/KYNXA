using System.Collections;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Views;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop;

public partial class App
{
    private async Task CheckWorkspaceAutoOpenAsync()
    {
        _gateway.EnableWorkspaceOpens = true;
        var project = ShellField<List<ProjectState>>("_projects").Single(item => item.Id == _gateway.ProjectId);
        var chat = project.Chats.Single(item => item.Id == _gateway.WorkChatId);
        Call("SelectProjectChat", project, chat);
        await SettleAsync();
        var tabs = Element<ConversationWorkTabs>("WorkTabs");
        var documents = Element<ConversationDocumentPanel>("DocumentPanel");
        var pendingReplies = ShellField<IDictionary>("_pendingReplies");
        var historicalMessage = chat.Messages.Last(item => item.Role == "assistant");
        var originalTools = historicalMessage.ToolActivities;
        object? pending = null;
        ChatMessageState? liveMessage = null;
        string historyUrl = _gateway.Address + "/workspace-history";
        string liveUrl = _gateway.Address + "/workspace-live";
        ToolActivity OpenReceipt(string id, string url) => new(id, "artifact.open", null, "completed", "Synthetic workspace page",
            Result: JsonSerializer.Serialize(new { kind = "browser", url, path = url, title = id, mimeType = "text/html" }));
        bool IsActivationRequested() => (bool)typeof(ConversationDocumentPanel)
            .GetField("_websiteActivationRequested", PrivateInstance)!.GetValue(documents)!;
        try
        {
            historicalMessage.ToolActivities = [OpenReceipt("historical-workspace-page", historyUrl)];
            _shell.ActiveMessages.Single(row => row.Message.Id == historicalMessage.Id).Refresh();
            Call("ApplyLayout");
            await WaitAsync(() => tabs.Items.Any(tab => tab.Target == historyUrl), "a historical resource receipt restores its webpage tab");
            await SettleAsync();
            Check(_gateway.ChromeOpenRequests == 0 && !IsActivationRequested(),
                "historical webpage restoration does not start a browser or request activation");
            tabs.Close(tabs.Items.Single(tab => tab.Target == historyUrl).Key);

            pending = Call("BeginPendingReply", chat.Id, "Open the synthetic workspace page", "fixture-provider", "fixture-model", "full", null)!;
            liveMessage = (ChatMessageState)pending.GetType().GetProperty("Message")!.GetValue(pending)!;
            var receipt = OpenReceipt("live-workspace-page", liveUrl);
            Call("AcceptToolActivity", pending, receipt);
            // The final SSE can remove pending ownership before deferred discovery finishes; keep that ordering exact.
            // 最后一条 SSE 可在异步发现完成前移除 pending；此处不刷新布局，精确复现该顺序。
            pendingReplies.Remove(chat.Id);
            liveMessage.Status = "completed";
            await WaitAsync(() => _gateway.ChromeOpenRequests == 1, "a completed live resource dispatches workspace opening after pending removal");
            Check(tabs.SelectedTab?.Target == liveUrl && IsActivationRequested() && NativeUi.IsVisible(documents),
                "the final live receipt selects and activates the right workspace without requiring the Open webpage button");

            Call("AcceptToolActivity", pending, receipt);
            Call("ApplyLayout");
            await SettleAsync();
            Check(_gateway.ChromeOpenRequests == 1 && tabs.Items.Count(tab => tab.Target == liveUrl) == 1,
                "duplicate completed receipts and later layout refresh do not open the same webpage twice");
            File.AppendAllText(ResultPath,
                "Workspace evidence: production Shell → ChromeWorkspaceApiClient → loopback POST; synthetic 503, no browser pixels or external website claim.\n");
        }
        finally
        {
            pendingReplies.Remove(chat.Id);
            if (pending is not null)
                ((CancellationTokenSource)pending.GetType().GetProperty("Cancellation")!.GetValue(pending)!).Dispose();
            ShellField<DispatcherTimer?>("_replyRefreshTimer")?.Stop();
            if (liveMessage is not null) chat.Messages.Remove(liveMessage);
            historicalMessage.ToolActivities = originalTools;
        }
    }
}
