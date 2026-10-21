using System.Text.Json;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.Views;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop;

public partial class App
{
    private RetrievalSettingsWindow? CurrentRetrievalWindow =>
        (RetrievalSettingsWindow?)typeof(ShellPage).GetField("_retrievalSettingsWindow", PrivateInstance)!.GetValue(_shell);

    private async Task CheckRetrievalNavigationAsync()
    {
        Check(CurrentRetrievalWindow is null, "the real knowledge navigation starts without an owned retrieval window");
        var knowledge = NativeUi.Descendants<Button>(_shell).Single(button =>
            AutomationProperties.GetName(button) == UiText.Get("知识库") && NativeUi.IsVisible(button));
        string draft = Prompt.Text;
        Guid? chatId = ActiveId;
        bool chatMode = _shell.ViewModel.IsChatMode;
        var messages = _shell.ActiveMessages.Select(row => row.Message).ToArray();
        string originalMessages = JsonSerializer.Serialize(messages);
        string originalCatalog = JsonSerializer.Serialize(_gateway.Catalog);
        int originalReads = _gateway.RetrievalReads;
        int writes = _gateway.Writes;
        RetrievalSettingsWindow? retrieval = null;
        int closed = 0;
        try
        {
            // Invoke the real sidebar button and let the production client read only the mock gateway.
            // 点击真实侧栏按钮，由生产客户端仅从模拟网关读取设置。
            NativeUi.Invoke(knowledge);
            await WaitAsync(() => CurrentRetrievalWindow?.Content is FrameworkElement { IsLoaded: true },
                "the actual knowledge button opens a loaded production retrieval settings window");
            retrieval = CurrentRetrievalWindow!;
            retrieval.Closed += (_, _) => closed++;
            var root = (FrameworkElement)retrieval.Content;
            await WaitAsync(() => _gateway.RetrievalReads == originalReads + 4 && !retrieval.HasPendingChanges &&
                NativeUi.Descendants<ComboBox>(root).Any(picker => AutomationProperties.GetAutomationId(picker) ==
                    "RetrievalLocalMode" && picker.SelectedItem is ComboBoxItem),
                "the production retrieval client finishes its four read-only global settings requests");
            var notice = NativeUi.ById<InfoBar>(root, "RetrievalSettingsNotice");
            Check(retrieval.ProjectId is null && !notice.IsOpen &&
                (NativeUi.ById<ComboBox>(root, "RetrievalLocalMode").SelectedItem as ComboBoxItem)?.Tag as string == "true" &&
                (NativeUi.ById<ComboBox>(root, "RetrievalWebMode").SelectedItem as ComboBoxItem)?.Tag as string == "off",
                "knowledge navigation renders authoritative synthetic global settings rather than the obsolete unavailable dialog");

            NativeUi.Invoke(knowledge);
            await SettleAsync();
            Check(ReferenceEquals(CurrentRetrievalWindow, retrieval) && closed == 0 &&
                _gateway.RetrievalReads == originalReads + 4 && _gateway.Writes == writes,
                "repeated knowledge navigation reuses the same native window without new reads or writes");
            Check(Prompt.Text == draft && ActiveId == chatId && _shell.ViewModel.IsChatMode == chatMode &&
                _shell.ActiveMessages.Select(row => row.Message).SequenceEqual(messages) &&
                JsonSerializer.Serialize(messages) == originalMessages,
                "opening and reusing retrieval settings preserve the active conversation, exact draft and message objects");

            // Close the actual secondary window; its production Closed handler must release the Shell owner reference.
            // 关闭真实附属窗口；生产 Closed 回调应释放 Shell 持有的窗口引用。
            retrieval.Close();
            await WaitAsync(() => CurrentRetrievalWindow is null && closed == 1,
                "closing retrieval settings clears its production owner reference exactly once");
            Check(Prompt.Text == draft && ActiveId == chatId && _shell.ViewModel.IsChatMode == chatMode &&
                JsonSerializer.Serialize(messages) == originalMessages && JsonSerializer.Serialize(_gateway.Catalog) == originalCatalog &&
                _gateway.RetrievalReads == originalReads + 4 && _gateway.Writes == writes && _gateway.Failure is null,
                "closing knowledge settings preserves drafts and the formal catalog without gateway writes");
        }
        finally
        {
            if (CurrentRetrievalWindow is { } remaining) remaining.CloseForOwner();
        }
    }
}
