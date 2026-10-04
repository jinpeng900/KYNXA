using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private static string ScreenshotTabKey(ConversationScreenshotSource source) =>
        $"screenshot/{source.ConversationId:D}/{source.MessageId:D}/{source.Tool.ToolCallId}";

    private void InitializeWorkTabs()
    {
        ScreenshotPanel.SetTabbedMode(true);
        WorkTabs.TabsChanged += (_, _) => { if (!_chatClosing) ApplyLayout(); };
    }

    private void SynchronizeWorkTabs()
    {
        var resources = new Dictionary<(Guid MessageId, string ToolCallId), ConversationWorkTab>();
        int imageNumber = 0;
        foreach (var source in ScreenshotPanel.Items)
            if (source.ConversationId == ActiveChatId)
                resources[(source.MessageId, source.Tool.ToolCallId)] =
                    new(ScreenshotTabKey(source), "screenshot", $"{UiText.Get("截图")} {++imageNumber}");
        var items = new List<ConversationWorkTab>();
        foreach (var message in ActiveMessages)
            foreach (var tool in message.ToolActivities)
                if (resources.Remove((message.Message.Id, tool.ToolCallId), out var tab)) items.Add(tab);
        // A live call can arrive before the message view model's next refresh.
        // 实际工具调用可能先于消息视图模型的下一次刷新到达。
        items.AddRange(resources.Values);
        WorkTabs.ShowConversation(ActiveChatId, items);
    }

    private void PresentSelectedWorkTab(bool sidebarVisible)
    {
        var selected = WorkTabs.SelectedTab;
        var screenshot = selected?.Kind == "screenshot"
            ? ScreenshotPanel.Items.FirstOrDefault(source => ScreenshotTabKey(source) == selected.Key) : null;
        bool showScreenshot = sidebarVisible && screenshot is not null;

        // Disable hidden viewers before switching their identity; they must not fetch or decode in the background.
        // 切换查看器身份之前禁用隐藏查看器，避免在后台继续获取或解码。
        if (!showScreenshot) ScreenshotPanel.SetPreviewEnabled(false);
        if (showScreenshot)
        {
            if (ScreenshotPanel.SelectedSource?.Identity != screenshot!.Identity)
                ScreenshotPanel.SelectSource(screenshot.MessageId, screenshot.Tool.ToolCallId);
            ScreenshotPanel.SetPreviewEnabled(true);
        }
        ScreenshotPanel.Visibility = showScreenshot ? Visibility.Visible : Visibility.Collapsed;
        WorkTabEmptyHint.Visibility = sidebarVisible && !showScreenshot ? Visibility.Visible : Visibility.Collapsed;
    }

    private void SuspendUnselectedWorkViewers(bool sidebarVisible)
    {
        var selected = WorkTabs.SelectedTab;
        var source = ScreenshotPanel.SelectedSource;
        if (!sidebarVisible || source is null || source.ConversationId != ActiveChatId || selected?.Key != ScreenshotTabKey(source))
            ScreenshotPanel.SetPreviewEnabled(false);
    }
}
