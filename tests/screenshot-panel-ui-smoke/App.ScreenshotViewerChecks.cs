using System.Text.Json;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.Views;
using MemoryUiSmoke;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Automation.Provider;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Graphics.Imaging;

namespace ScreenshotPanelUiSmoke;

public partial class App
{
    private async Task CheckResponsiveScreenshotAsync()
    {
        using var panel = new ConversationScreenshotsPanel { Margin = new Thickness(14, 42, 14, 14) };
        var api = new FakeScreenshotApi(); panel.ConfigureApi(api);
        Grid.SetColumn(panel, 1); _layout.Children.Add(panel);
        var conversation = Guid.NewGuid(); var tool = Screenshot("responsive-large");
        string large = await EncodeAsync(BitmapEncoder.PngEncoderId, 3000, 1500);
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(1500, 840));
        _layout.ColumnDefinitions[1].Width = new GridLength(330);
        panel.ShowConversation(conversation, [Message(conversation, tool)]); panel.SetPreviewEnabled(true);
        try
        {
            await WaitAsync(() => api.Reads.Count == 1, "responsive gallery loads one bounded original archive");
            api.Reads[0].Completion.TrySetResult(new(JsonSerializer.SerializeToElement(new
                { content = new[] { new { type = "image", mimeType = "image/png", data = large } } })));
            await WaitAsync(() => NativeUi.Descendants<Image>(panel).Any(item => item.Name == "ScreenshotPanelImage"),
                "responsive image is realized after its native button template loads");
            var image = NativeUi.ByName<Image>(panel, "ScreenshotPanelImage");
            await WaitAsync(() => image.Source is not null && image.ActualWidth > 0, "narrow large screenshot renders proportionally");
            double narrow = image.ActualWidth;
            var original = image.Source;
            _layout.ColumnDefinitions[1].Width = new GridLength(1260);
            await WaitAsync(() => image.ActualWidth > narrow * 2 && !ReferenceEquals(original, image.Source),
                "widening the sidebar enlarges the image and upgrades decoded pixels after resizing settles");
            Check(image.ActualHeight > 230 && Math.Abs(image.ActualWidth / image.ActualHeight - 2) < 0.02 &&
                image.ActualHeight <= panel.ActualHeight - 60, "large screenshot uses available height without the former 230-pixel cap or distorted aspect ratio");
            Check(api.Reads.Count == 1 && image.Source is BitmapImage bitmap && bitmap.DecodePixelWidth >= 2048,
                "resolution upgrade reuses bounded archive bytes rather than repeating an API or screenshot call");
            var surface = NativeUi.ByName<Border>(panel, "ScreenshotPanelSurface");
            double top = surface.TransformToVisual(panel).TransformPoint(new Windows.Foundation.Point()).Y;
            Check(top >= 0 && Math.Abs(top + surface.ActualHeight / 2 - panel.ActualHeight / 2 - 12) < 2,
                "single screenshot is centered slightly below the content-region midpoint");
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "panel-responsive-large.png"));
            var upgraded = image.Source;
            for (int width = 1180; width < 1270; width += 10) _layout.ColumnDefinitions[1].Width = new GridLength(width);
            await Task.Delay(300);
            Check(api.Reads.Count == 1 && ReferenceEquals(upgraded, image.Source), "dragging within a decode tier retains the image and performs no repeated reads");
            _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(880, 500));
            _layout.ColumnDefinitions[1].Width = new GridLength(290); await SettleAsync();
            Check(image.ActualWidth <= panel.ActualWidth && image.ActualHeight <= panel.ActualHeight && api.Reads.Count == 1,
                "narrowing fits the same cached screenshot into the remaining viewport");
        }
        finally
        {
            panel.Dispose(); _layout.Children.Remove(panel);
            _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(880, 500)); _layout.ColumnDefinitions[1].Width = new GridLength(290);
        }
    }

    private async Task CheckScreenshotViewerAsync()
    {
        var api = new FakeScreenshotApi(); var conversation = Guid.NewGuid();
        var tool = Screenshot("fullscreen-browser") with { Name = "mcp.custom-browser.browser_take_screenshot" };
        var source = new ConversationScreenshotSource(conversation, Guid.NewGuid(), tool);
        string original = await EncodeAsync(BitmapEncoder.PngEncoderId, 3840, 2160);
        using (var viewer = new ScreenshotViewerWindow(api, source))
        {
            Task showing = viewer.ShowAsync();
            await WaitAsync(() => viewer.Content is Grid grid && grid.XamlRoot is not null && api.Reads.Count == 1,
                "production independent full-screen viewer loads the browser receipt through its archive API");
            var root = (Grid)viewer.Content;
            api.Reads[0].Completion.TrySetResult(new(JsonSerializer.SerializeToElement(new
                { content = new[] { new { type = "image", mimeType = "image/png", data = original } } })));
            await WaitAsync(() => NativeUi.Descendants<Image>(root).Any(item => item.Name == "ScreenshotViewerImage"),
                "full-screen image viewport completes native realization");
            var image = NativeUi.ByName<Image>(root, "ScreenshotViewerImage");
            var scroll = NativeUi.ByName<ScrollViewer>(root, "ScreenshotViewerViewport");
            await WaitAsync(() => image.Source is not null && scroll.ViewportWidth > 0 && scroll.ZoomFactor < 1,
                "original screenshot initially fits the full-screen viewport");
            Check(viewer.AppWindow.Presenter.Kind == AppWindowPresenterKind.FullScreen && image.Source is BitmapImage bitmap &&
                bitmap.PixelWidth == 3840 && bitmap.PixelHeight == 2160 && bitmap.DecodePixelWidth == 0,
                "full-screen viewer decodes all original archive pixels rather than upscaling its 1024 thumbnail");
            NativeUi.Invoke(NativeUi.ByName<Button>(root, "ScreenshotViewerActual"));
            await WaitAsync(() => Math.Abs(scroll.ZoomFactor - 1) < 0.001, "actual-size button selects 100-percent zoom");
            Check(Math.Abs(image.Width * root.XamlRoot.RasterizationScale - 3840) < 0.01 &&
                Math.Abs(image.Height * root.XamlRoot.RasterizationScale - 2160) < 0.01,
                "100-percent mode maps original image pixels to physical screen pixels at the current DPI");
            using (var input = new ScreenshotFixtureInput(viewer))
            {
                if (input.UsesPhysicalInput)
                {
                    input.Wheel(root, scroll, 120);
                    await WaitAsync(() => scroll.ZoomFactor > 1.05, "native mouse wheel zooms the original image");
                    await input.DragAsync(root, scroll, -120, -90);
                    await WaitAsync(() => scroll.HorizontalOffset > 50 && scroll.VerticalOffset > 30,
                        "native mouse drag pans the enlarged image");
                    double horizontal = scroll.HorizontalOffset;
                    input.Move(root, scroll, 100, 40); await SettleAsync();
                    Check(Math.Abs(scroll.HorizontalOffset - horizontal) < 1, "releasing the mouse stops image panning");
                }
                else
                {
                    _checks.Add("SKIP physical wheel/drag/Escape: Windows refused fixture foreground; no global inputs were sent.");
                    var peer = FrameworkElementAutomationPeer.CreatePeerForElement(scroll);
                    if (peer?.GetPattern(PatternInterface.Transform) is ITransformProvider2 zoom && zoom.CanZoom)
                    {
                        zoom.Zoom(120);
                        await WaitAsync(() => scroll.ZoomFactor > 1.05, "native UI Automation zooms the original image independently of mouse focus");
                    }
                    if (peer?.GetPattern(PatternInterface.Scroll) is IScrollProvider pan)
                    {
                        pan.SetScrollPercent(50, 50);
                        await WaitAsync(() => scroll.HorizontalOffset > 0 && scroll.VerticalOffset > 0,
                            "native UI Automation pans the original image independently of mouse focus");
                    }
                }
                NativeUi.Invoke(NativeUi.ByName<Button>(root, "ScreenshotViewerFit"));
                await WaitAsync(() => scroll.ZoomFactor < 1, "fit button returns the original image to viewport size");
                UiText.Initialize("en"); await SettleAsync();
                Check(viewer.Title == "Screenshot preview" && NativeUi.ByName<Button>(root, "ScreenshotViewerFit").Content as string == "Fit to window" &&
                    api.Reads.Count == 1, "full-screen language switches without changing the archive or image");
                await NativeWindowCapture.CaptureAsync(viewer, Path.Combine(_directory, "viewer-fullscreen-original.png"));
                if (input.UsesPhysicalInput) input.Escape();
                else NativeUi.Invoke(NativeUi.ByName<Button>(root, "ScreenshotViewerClose"));
                await WaitAsync(() => showing.IsCompleted, input.UsesPhysicalInput ?
                    "native Escape closes the full-screen image viewer" : "native close button closes the full-screen image viewer");
            }
            await showing;
            Check(image.Source is null, "closing the viewer releases decoded original pixels");
        }
        UiText.Initialize("zh-CN"); _window.Activate();
        using var cancellation = new CancellationTokenSource();
        using var cancelled = new ScreenshotViewerWindow(api, source, cancellation.Token);
        Task pending = cancelled.ShowAsync();
        await WaitAsync(() => api.Reads.Count == 2, "pending viewer has one formal archive read");
        var cancelledRoot = (Grid)cancelled.Content;
        cancellation.Cancel(); await WaitAsync(() => pending.IsCompleted, "conversation cancellation closes a loading full-screen viewer");
        api.Reads[1].Completion.TrySetResult(new(JsonSerializer.SerializeToElement(new
            { content = new[] { new { type = "image", mimeType = "image/png", data = original } } })));
        await SettleAsync();
        Check(api.Reads[1].Token.IsCancellationRequested && NativeUi.Descendants<Image>(cancelledRoot).All(item => item.Source is null),
            "late archive response cannot repopulate a cancelled viewer");
        await pending; _window.Activate();
        using var loadedCancellation = new CancellationTokenSource();
        using var loaded = new ScreenshotViewerWindow(api, source, loadedCancellation.Token);
        Task loadedTask = loaded.ShowAsync();
        await WaitAsync(() => api.Reads.Count == 3, "second viewer reads its own original archive");
        api.Reads[2].Completion.TrySetResult(new(JsonSerializer.SerializeToElement(new
            { content = new[] { new { type = "image", mimeType = "image/png", data = _png } } })));
        await WaitAsync(() => NativeUi.Descendants<Image>((Grid)loaded.Content).Any(item => item.Source is not null),
            "second viewer completes loading before cancellation");
        loadedCancellation.Cancel(); await WaitAsync(() => loadedTask.IsCompleted, "conversation cancellation also closes an already loaded viewer");
        await loadedTask; _window.Activate();
        string tall = await EncodeAsync(BitmapEncoder.PngEncoderId, 400, 16000);
        using var longViewer = new ScreenshotViewerWindow(api, source);
        Task longTask = longViewer.ShowAsync();
        await WaitAsync(() => api.Reads.Count == 4, "long-page viewer reads its original screenshot archive");
        api.Reads[3].Completion.TrySetResult(new(JsonSerializer.SerializeToElement(new
            { content = new[] { new { type = "image", mimeType = "image/png", data = tall } } })));
        var longRoot = (Grid)longViewer.Content;
        await WaitAsync(() => NativeUi.Descendants<Image>(longRoot).Any(item => item.Source is not null), "long-page original pixels render");
        var longImage = NativeUi.ByName<Image>(longRoot, "ScreenshotViewerImage");
        var longScroll = NativeUi.ByName<ScrollViewer>(longRoot, "ScreenshotViewerViewport");
        await WaitAsync(() => longImage.Height * longScroll.ZoomFactor <= longScroll.ViewportHeight + 1 && longScroll.ViewportHeight > 0,
            "Fit displays the entire long page despite WinUI's native minimum zoom factor");
        NativeUi.Invoke(NativeUi.ByName<Button>(longRoot, "ScreenshotViewerActual"));
        await WaitAsync(() => Math.Abs(longScroll.ZoomFactor - 1) < 0.001 && Math.Abs(longImage.Height * longRoot.XamlRoot.RasterizationScale - 16000) < 1,
            "100-percent mode restores original long-page dimensions without reducing its pixel data");
        NativeUi.Invoke(NativeUi.ByName<Button>(longRoot, "ScreenshotViewerClose")); await longTask; _window.Activate();
    }
}
