using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Graphics.Imaging;

namespace ScreenshotPanelUiSmoke;

public partial class App
{
    private async Task CheckBrowserScreenshotsAsync()
    {
        using var panel = new ConversationScreenshotsPanel { Margin = new Thickness(14, 42, 14, 14) };
        var api = new FakeScreenshotApi(); panel.ConfigureApi(api);
        Grid.SetColumn(panel, 1); _layout.Children.Add(panel);
        var playwright = Screenshot("playwright-png") with { Name = "mcp.custom-browser.browser_take_screenshot" };
        var chrome = Screenshot("chrome-jpeg") with { Name = "mcp.browser-debug.take_screenshot" };
        var remote = Screenshot("remote-png") with { Name = "mcp.remote-browser.browser_take_screenshot" };
        var native = Screenshot("native-png"); var conversation = Guid.NewGuid();
        var row = Message(conversation, native, playwright, chrome, remote);
        try
        {
            var sources = ConversationScreenshotSources.Collect(conversation,
                [new(conversation, row.Message.Id, "assistant", row.ToolActivities)]);
            Check(sources.Select(source => source.Tool).SequenceEqual([native, playwright, chrome, remote]),
                "native, custom-named Playwright, Chrome and remote-browser screenshot receipts share the gallery in formal order");
            Check(!ConversationScreenshotSources.IsScreenshotTool("mcp.custom-browser.read_file") &&
                !ConversationScreenshotSources.IsScreenshotTool("user.browser_take_screenshot"),
                "ordinary tools and user-like text do not trigger archive screenshot downloads");
            panel.ShowConversation(conversation, [row]); panel.SetPreviewEnabled(true);
            await WaitAsync(() => api.Reads.Count == 1 && panel.IsLoaded, "remote-browser screenshot reads its local result archive on demand");
            var image = NativeUi.ByName<Image>(panel, "ScreenshotPanelImage");
            api.Reads[0].Completion.TrySetResult(new ToolResultResponse(JsonSerializer.SerializeToElement(new
            {
                content = new object[] {
                    new { type = "resource", resource = new { uri = "https://browser.example.invalid/shot.png", mimeType = "image/png", blob = _png } },
                    new { type = "image", mimeType = "image/png", data = _png }
                }
            })));
            await WaitAsync(() => image.Source is not null, "remote image block renders below the existing sidebar header without downloading its URI");
            Check(api.Reads[0].Reference == remote.ResultRef && NativeUi.ByName<TextBlock>(panel, "ScreenshotPanelPosition").Text == "4 / 4",
                "gallery selected latest remote screenshot retains chat and receipt ownership");
            NativeUi.Invoke(NativeUi.ByName<Button>(panel, "ScreenshotPanelPrevious"));
            await WaitAsync(() => api.Reads.Count == 2, "previous screenshot selects Chrome's completed receipt");
            string jpeg = await EncodeAsync(BitmapEncoder.JpegEncoderId, 64, 32);
            api.Reads[1].Completion.TrySetResult(new ToolResultResponse(JsonSerializer.SerializeToElement(new
            { content = new object[] { new { type = "text", text = "Saved screenshot." }, new { type = "image", mimeType = "image/jpeg", data = jpeg } } })));
            await WaitAsync(() => image.Source is not null, "Chrome typed JPEG becomes a proportional screenshot thumbnail");
            ToolResultRequest? opened = null; panel.ScreenshotOpenRequested += (_, request) => opened = request;
            NativeUi.Invoke(NativeUi.ByName<Button>(panel, "ScreenshotPanelOpen"));
            Check(opened?.Tool == chrome && opened.ConversationId == conversation, "browser screenshot opens with its own formal receipt");
            using var viewer = new ToolResultDialog(_layout.XamlRoot, api, conversation, chrome);
            Task showing = viewer.ShowAsync();
            await WaitAsync(() => api.Reads.Count == 3, "browser screenshot viewer directly loads archived media rather than JSON pages");
            api.Reads[2].Completion.TrySetResult(new ToolResultResponse(JsonSerializer.SerializeToElement(new
            { content = new[] { new { type = "image", mimeType = "image/jpeg", data = jpeg } } })));
            await WaitAsync(() => NativeUi.Descendants<Image>(viewer.Dialog).Any(item => item.Source is not null),
                "actual screenshot dialog renders the browser JPEG at full preview size");
            Check(viewer.Dialog.Title as string == "截图预览" &&
                !NativeUi.Descendants<TextBox>(viewer.Dialog).Any(item => item.Name == "ToolResultText"),
                "browser screenshot dialog uses the concise screenshot view and hides result JSON");
            viewer.Dialog.Hide(); await showing;
            NativeUi.Invoke(NativeUi.ByName<Button>(panel, "ScreenshotPanelPrevious"));
            await WaitAsync(() => api.Reads.Count == 4, "Playwright file screenshot is separately read from its archived image receipt");
            api.Reads[3].Completion.TrySetResult(new ToolResultResponse(JsonSerializer.SerializeToElement(new
            { content = new object[] { new { type = "text", text = "[Screenshot of viewport](./shot.png)" }, new { type = "image", mimeType = "image/png", data = _png } } })));
            await WaitAsync(() => image.Source is not null, "Playwright preserved Markdown plus appended PNG renders without a file or URL fallback");
            Check(api.Reads[3].Reference == playwright.ResultRef && row.Message.Content == "已读取页面并完成回复。",
                "browser screenshots never alter the final assistant text or attachment identity");
        }
        finally { panel.Dispose(); _layout.Children.Remove(panel); }
    }
}
