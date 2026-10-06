using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.Views;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Automation.Provider;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;

namespace KYNXA_Desktop;

public partial class App
{
    private T ShellField<T>(string name) => (T)typeof(ShellPage).GetField(name, PrivateInstance)!.GetValue(_shell)!;

    private void SetShellField(string name, object? value) => typeof(ShellPage).GetField(name, PrivateInstance)!.SetValue(_shell, value);

    private bool WorkPanelVisible => NativeUi.IsVisible(Element<Border>("WorkContextPanel"));

    private async Task CheckPanelPolishAsync()
    {
        var projects = ShellField<List<ProjectState>>("_projects");
        var workProject = projects.Single(project => project.Id == _gateway.ProjectId);
        var secondProject = projects.Single(project => project.Id == _gateway.SecondProjectId);
        var workChat = workProject.Chats.Single(chat => chat.Id == _gateway.WorkChatId);
        var layout = ShellField<LayoutState>("_layout");
        bool originalPreviewVisible = layout.PreviewVisible;
        double originalPreviewWidth = layout.PreviewWidth;
        string originalWorkDraft = Prompt.Text;
        object? originalStandalone = typeof(ShellPage).GetField("_activeStandaloneChat", PrivateInstance)!.GetValue(_shell);
        string originalChatDraft = ShellField<string>("_chatDraft");
        var assistantMessage = workChat.Messages.Last(message => message.Role == "assistant");
        var originalTools = assistantMessage.ToolActivities;
        var screenshots = Element<ConversationScreenshotsPanel>("ScreenshotPanel");
        var tabs = Element<ConversationWorkTabs>("WorkTabs");
        try
        {
            // Seed a deliberate user width in isolated UI storage, not a formal conversation or gateway setting.
            // 在隔离 UI 存储中设定用户宽度，不修改正式会话或网关设置。
            layout.PreviewVisible = true;
            layout.PreviewWidth = 432;
            Call("SaveLayout");
            Call("SelectWorkspaceProject", workProject);
            await SettleAsync();
            var saved = new LayoutStateService().Load();
            Check(ActiveId is null && Element<MountedWorkspaceHeader>("MountedWorkspace").HasMountedFolder && !WorkPanelVisible &&
                NativeUi.IsVisible(Element<Button>("OpenWorkPanelButton")) && saved.PreviewVisible && saved.PreviewWidth == 432,
                "a mounted workspace with no active conversation or preview hides the right column while retaining explicit user preferences");
            NativeUi.Invoke(Element<Button>("OpenWorkPanelButton"));
            await WaitAsync(() => WorkPanelVisible && NativeUi.IsVisible(Element<TextBlock>("WorkTabEmptyHint")),
                "the actual Open button reveals a useful empty preview hint");
            Check(Math.Abs(Element<Border>("WorkContextPanel").ActualWidth - 432) < 2,
                "an explicitly opened empty preview uses the saved user width");
            NativeUi.SetText(Prompt, "project-only draft");
            await SettleAsync();
            Check(WorkPanelVisible && NativeUi.IsVisible(Element<TextBlock>("WorkTabEmptyHint")),
                "editing a draft in the same project and null conversation does not clear its explicit empty-panel request");

            Call("SelectWorkspaceProject", secondProject);
            await SettleAsync();
            saved = new LayoutStateService().Load();
            Check(ActiveId is null && !WorkPanelVisible && NativeUi.IsVisible(Element<Button>("OpenWorkPanelButton")) &&
                saved.PreviewVisible && saved.PreviewWidth == 432,
                "switching between two project-only null conversations clears the temporary open request without changing preview preferences");
            NativeUi.Invoke(Element<Button>("OpenWorkPanelButton"));
            await WaitAsync(() => WorkPanelVisible, "the second project can explicitly open its own empty panel");

            // Retain the real standalone object; only the fixture's temporary null selection exercises the equal-ID boundary.
            // 保留真实独立会话对象，仅用夹具的临时空选择检验两侧 ID 均为空的边界。
            SetShellField("_activeStandaloneChat", null);
            Call("SetPrimaryMode", true, true);
            await SettleAsync();
            Check(_shell.ViewModel.IsChatMode && ActiveId is null && !WorkPanelVisible,
                "switching to null standalone chat mode hides a project-only empty panel");
            Call("SetPrimaryMode", false, true);
            await SettleAsync();
            Check(!_shell.ViewModel.IsChatMode && ActiveId is null && !WorkPanelVisible && layout.PreviewVisible && layout.PreviewWidth == 432,
                "returning to null work mode cannot revive an empty request from the previous mode");
            SetShellField("_activeStandaloneChat", originalStandalone);
            SetShellField("_chatDraft", originalChatDraft);
            Call("SelectProjectChat", workProject, workChat);
            NativeUi.SetText(Prompt, originalWorkDraft);
            await SettleAsync();
            Check(ActiveId == _gateway.WorkChatId && !WorkPanelVisible && !tabs.HasOpenTabs,
                "a saved work conversation with text but no resources gives the full column back to the transcript");

            ResizeInDips(800, 720);
            await WaitAsync(() => Element<Grid>("MainRegion").ActualWidth < 900 && !WorkPanelVisible,
                "the compact Shell cannot allocate a preview column");
            int readsBeforeDiscovery = _gateway.ScreenshotReads;
            var reference = _gateway.SetScreenshotArchive(await EncodePanelFixturePngAsync());
            var receipt = new ToolActivity("panel-polish-screenshot", "computer.screenshot", null, "completed", "Synthetic preview",
                ResultRef: reference);
            assistantMessage.ToolActivities = [receipt];
            _shell.ActiveMessages.Single(row => row.Message.Id == assistantMessage.Id).Refresh();
            await WaitAsync(() => screenshots.Items.Count == 1 && tabs.HasOpenTabs,
                "a formal-shaped completed receipt is discovered and tabbed while the preview is hidden");
            await SettleAsync();
            Check(screenshots.Items[0].ConversationId == _gateway.WorkChatId && screenshots.Items[0].MessageId == assistantMessage.Id &&
                screenshots.Items[0].Tool.ResultRef == reference && _gateway.ScreenshotReads == readsBeforeDiscovery && !WorkPanelVisible,
                "compact resource discovery retains stable message/chat/result identity without reading or decoding an image");
            ResizeInDips(1440, 900);
            await WaitAsync(() => WorkPanelVisible && screenshots.IsLoaded &&
                NativeUi.Descendants<Image>(screenshots).Any(image => image.Source is not null),
                "restoring wide layout automatically displays the already discovered screenshot through the real archive client");
            await SettleAsync();
            Check(_gateway.ScreenshotReads == readsBeforeDiscovery + 1 && Math.Abs(Element<Border>("WorkContextPanel").ActualWidth - 432) < 2,
                "synchronous resource opening fetches exactly one synthetic archive and restores the saved preview width");
            await CapturePresentedAsync("shell-polish-screenshot-wide.png");

            NativeUi.Invoke(Element<Button>("CloseWorkPanelButton"));
            await WaitAsync(() => !WorkPanelVisible, "the actual Close button hides the preview");
            saved = new LayoutStateService().Load();
            Check(!saved.PreviewVisible && saved.PreviewWidth == 432 && tabs.HasOpenTabs && screenshots.Items.Count == 1 &&
                NativeUi.Descendants<Image>(screenshots).All(image => image.Source is null),
                "explicit close persists only the closed preference, retains resources and width, and releases the visible image");
            _shell.ActiveMessages.Single(row => row.Message.Id == assistantMessage.Id).Refresh();
            ResizeInDips(800, 720);
            await SettleAsync();
            ResizeInDips(1440, 900);
            await SettleAsync();
            Check(!WorkPanelVisible && !layout.PreviewVisible && layout.PreviewWidth == 432 && _gateway.ScreenshotReads == readsBeforeDiscovery + 1,
                "message refresh and narrow/wide resizing cannot reopen a user-closed panel or perform another archive read");
            NativeUi.Invoke(Element<Button>("OpenWorkPanelButton"));
            await WaitAsync(() => WorkPanelVisible && NativeUi.Descendants<Image>(screenshots).Any(image => image.Source is not null),
                "explicit opening restores the existing screenshot tab");
            string tabKey = tabs.SelectedTab!.Key;
            var closeTab = NativeUi.Descendants<Button>(tabs).Single(button => button.Name == "WorkTabClose" && button.Tag as string == tabKey);
            NativeUi.Invoke(closeTab);
            await WaitAsync(() => !tabs.HasOpenTabs && !WorkPanelVisible,
                "closing the last tab after opening an existing resource releases the empty right column");
            _shell.ActiveMessages.Single(row => row.Message.Id == assistantMessage.Id).Refresh();
            Call("ApplyLayout");
            await SettleAsync();
            Check(tabs.HasItems && !tabs.HasOpenTabs && !WorkPanelVisible && layout.PreviewVisible && layout.PreviewWidth == 432 &&
                _gateway.ScreenshotReads == readsBeforeDiscovery + 1,
                "refreshing the same receipt preserves the user's closed tab without changing the panel preference or reading again");

            NativeUi.Invoke(Element<Button>("OpenWorkPanelButton"));
            await WaitAsync(() => WorkPanelVisible && NativeUi.IsVisible(Element<TextBlock>("WorkTabEmptyHint")),
                "the empty panel can be explicitly opened to reach the existing reopen menu");
            NativeUi.Invoke(NativeUi.ByName<Button>(tabs, "WorkTabsReopen"));
            await WaitAsync(() => VisualTreeHelper.GetOpenPopupsForXamlRoot(_shell.XamlRoot).Any(popup => popup.IsOpen && popup.Child is not null &&
                NativeUi.Descendants<MenuFlyoutItem>(popup.Child).Any(item => item.Tag as string == tabKey)),
                "the production tab reopen menu exposes the retained screenshot identity");
            var reopenItem = VisualTreeHelper.GetOpenPopupsForXamlRoot(_shell.XamlRoot).Where(popup => popup.IsOpen && popup.Child is not null)
                .SelectMany(popup => NativeUi.Descendants<MenuFlyoutItem>(popup.Child!)).Single(item => item.Tag as string == tabKey);
            var peer = FrameworkElementAutomationPeer.CreatePeerForElement(reopenItem);
            if (peer?.GetPattern(PatternInterface.Invoke) is not IInvokeProvider invoke)
                throw new InvalidOperationException("The production tab reopen menu has no native Invoke pattern.");
            invoke.Invoke();
            await WaitAsync(() => tabs.SelectedTab?.Key == tabKey && NativeUi.Descendants<Image>(screenshots).Any(image => image.Source is not null),
                "explicit native menu reopening restores the exact screenshot and its cached pixels");
            Check(ActiveId == _gateway.WorkChatId && Prompt.Text == originalWorkDraft && _gateway.Writes == 0 && _gateway.Catalog.Revision == 7,
                "preview actions preserve the work draft and conversation/catalog identity without formal gateway writes");
        }
        finally
        {
            assistantMessage.ToolActivities = originalTools;
            SetShellField("_activeStandaloneChat", originalStandalone);
            SetShellField("_chatDraft", originalChatDraft);
            // A real project transition clears temporary empty opening before returning to the original work chat.
            // 通过真实项目切换清除临时空面板请求，再回到原工作会话。
            Call("SelectWorkspaceProject", secondProject);
            Call("SelectProjectChat", workProject, workChat);
            NativeUi.SetText(Prompt, originalWorkDraft);
            layout.PreviewVisible = originalPreviewVisible;
            layout.PreviewWidth = originalPreviewWidth;
            Call("ApplyLayout");
            Call("SaveLayout");
            ResizeInDips(1440, 900);
            await SettleAsync();
        }
        Check(!_shell.ViewModel.IsChatMode && ActiveId == _gateway.WorkChatId && Prompt.Text == originalWorkDraft && !WorkPanelVisible &&
            _gateway.Writes == 0, "the panel fixture restores the original work context, draft, tools and user preview settings");
        await CapturePresentedAsync("shell-polish-wide-after-preview.png");
    }

    private static async Task<string> EncodePanelFixturePngAsync()
    {
        const uint width = 48, height = 24;
        byte[] pixels = new byte[checked((int)(width * height * 4))];
        for (uint y = 0; y < height; y++)
            for (uint x = 0; x < width; x++)
            {
                int offset = checked((int)((y * width + x) * 4));
                pixels[offset] = x < width / 2 ? (byte)232 : (byte)204;
                pixels[offset + 1] = y < height / 2 ? (byte)221 : (byte)192;
                pixels[offset + 2] = 180;
                pixels[offset + 3] = 255;
            }
        using var stream = new InMemoryRandomAccessStream();
        var encoder = await BitmapEncoder.CreateAsync(BitmapEncoder.PngEncoderId, stream);
        encoder.SetPixelData(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Ignore, width, height, 96, 96, pixels);
        await encoder.FlushAsync();
        stream.Seek(0);
        using var output = new MemoryStream();
        await stream.AsStreamForRead().CopyToAsync(output);
        return Convert.ToBase64String(output.ToArray());
    }
}
