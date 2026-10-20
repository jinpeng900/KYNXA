using System.Text.Json;
using KYNXA_Desktop.Controls;
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

    private Window? CurrentRetrievalOwner =>
        (Window?)typeof(ShellPage).GetField("_storageSettingsWindow", PrivateInstance)!.GetValue(_shell);

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
        Window? settings = null;
        int retrievalClosed = 0;
        int settingsClosed = 0;
        try
        {
            // Invoke the real sidebar button and let the production client read only the mock gateway.
            // 点击真实侧栏按钮，由生产客户端仅从模拟网关读取设置。
            NativeUi.Invoke(knowledge);
            await WaitAsync(() => CurrentRetrievalWindow is not null &&
                CurrentRetrievalOwner?.Content is FrameworkElement { IsLoaded: true } content &&
                NativeUi.Descendants<FrameworkElement>(content).Any(element => element.IsLoaded &&
                    AutomationProperties.GetAutomationId(element) == "SettingsPage_retrieval" && NativeUi.IsVisible(element)),
                "the actual knowledge button loads the retrieval page inside the production settings window");
            retrieval = CurrentRetrievalWindow!;
            settings = CurrentRetrievalOwner!;
            retrieval.Closed += (_, _) => retrievalClosed++;
            settings.Closed += (_, _) => settingsClosed++;
            var root = (FrameworkElement)settings.Content;
            var workspace = NativeUi.Descendants<SettingsWorkspace>(root).Single();
            var page = NativeUi.ById<FrameworkElement>(root, "SettingsPage_retrieval");
            Check(retrieval.IsEmbedded && retrieval.Content is null && workspace.SelectedKey == "retrieval" &&
                NativeUi.IsVisible(page),
                "knowledge navigation embeds the real retrieval controller in the selected settings page");
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
            Check(ReferenceEquals(CurrentRetrievalWindow, retrieval) && ReferenceEquals(CurrentRetrievalOwner, settings) &&
                ReferenceEquals(page, NativeUi.ById<FrameworkElement>(root, "SettingsPage_retrieval")) &&
                workspace.SelectedKey == "retrieval" && NativeUi.IsVisible(page) && retrievalClosed == 0 && settingsClosed == 0 &&
                _gateway.RetrievalReads == originalReads + 4 && _gateway.Writes == writes,
                "repeated knowledge navigation reuses the same settings owner, retrieval controller and page without new reads or writes");
            Check(Prompt.Text == draft && ActiveId == chatId && _shell.ViewModel.IsChatMode == chatMode &&
                _shell.ActiveMessages.Select(row => row.Message).SequenceEqual(messages) &&
                JsonSerializer.Serialize(messages) == originalMessages,
                "opening and reusing retrieval settings preserve the active conversation, exact draft and message objects");

            // Close the visible settings owner; its production handler disposes the embedded controller and clears both references.
            // 关闭可见设置窗口，由生产回调释放内嵌检索控制器并清除两处引用。
            NativeUi.RequestClose(settings);
            await WaitAsync(() => CurrentRetrievalOwner is null && CurrentRetrievalWindow is null &&
                settingsClosed == 1 && retrievalClosed == 1,
                "closing the settings owner releases its embedded retrieval controller and both Shell references exactly once");
            Check(Prompt.Text == draft && ActiveId == chatId && _shell.ViewModel.IsChatMode == chatMode &&
                JsonSerializer.Serialize(messages) == originalMessages && JsonSerializer.Serialize(_gateway.Catalog) == originalCatalog &&
                _gateway.RetrievalReads == originalReads + 4 && _gateway.Writes == writes && _gateway.Failure is null,
                "closing knowledge settings preserves drafts and the formal catalog without gateway writes");
        }
        finally
        {
            if (CurrentRetrievalOwner is { } remaining) remaining.Close();
        }
    }
}
