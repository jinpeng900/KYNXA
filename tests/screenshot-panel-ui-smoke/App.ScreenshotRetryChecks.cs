using System.Text.Json;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.Views;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Graphics.Imaging;

namespace ScreenshotPanelUiSmoke;

public partial class App
{
    private async Task CheckScreenshotRetryAsync()
    {
        string language = UiText.Language;
        var api = new FakeScreenshotApi();
        var source = new ConversationScreenshotSource(Guid.NewGuid(), Guid.NewGuid(), Screenshot("retry-same-archive"));
        string png = await EncodeAsync(BitmapEncoder.PngEncoderId, 1280, 640);
        try
        {
            using (var viewer = new ScreenshotViewerWindow(api, source))
            {
                var shown = viewer.ShowAsync();
                await WaitAsync(() => api.Reads.Count == 1 && ((Grid)viewer.Content).XamlRoot is not null,
                    "a retry fixture first reads one owned screenshot archive");
                var root = (Grid)viewer.Content;
                var retry = NativeUi.ByName<Button>(root, "ScreenshotViewerRetry");
                Check(retry.Visibility == Visibility.Collapsed && !retry.IsEnabled,
                    "a normally loading screenshot does not offer an enabled retry action");
                api.Reads[0].Completion.TrySetException(new IOException("Synthetic archive temporarily unavailable."));
                await WaitAsync(() => retry.Visibility == Visibility.Visible && retry.IsEnabled &&
                    NativeUi.ByName<TextBlock>(root, "ScreenshotViewerNotice").Text == UiText.Get("图片无法预览。"),
                    "a real read failure reveals a retry action and truthful localized notice");
                Check(retry.FocusState != FocusState.Unfocused && !NativeUi.ByName<Button>(root, "ScreenshotViewerActual").IsEnabled,
                    "failed reading focuses the reachable retry action without enabling image controls");
                UiText.Initialize("en");
                await SettleAsync();
                Check(retry.Content?.ToString() == UiText.Get("重新读取") &&
                    AutomationProperties.GetName(retry) == UiText.Get("重新读取已保存的截图") && api.Reads.Count == 1,
                    "retry text switches language without rereading or executing a screenshot");
                await NativeWindowCapture.CaptureAsync(viewer, Path.Combine(_directory, "viewer-read-failure-retry-en.png"));
                NativeUi.Invoke(retry);
                await WaitAsync(() => api.Reads.Count == 2 && !retry.IsEnabled,
                    "explicit retry starts exactly one archive read and disables duplicate input while it is pending");
                Check(api.Reads[1].Conversation == source.ConversationId && api.Reads[1].Reference == source.Tool.ResultRef &&
                    NativeUi.ByName<TextBlock>(root, "ScreenshotViewerNotice").Text == UiText.Get("正在读取工具结果…"),
                    "retry uses the same conversation and formal result reference and restores the loading state");
                api.Reads[1].Completion.TrySetResult(new(JsonSerializer.SerializeToElement(new
                    { content = new[] { new { type = "image", mimeType = "image/png", data = png } } })));
                await WaitAsync(() => NativeUi.ByName<Image>(root, "ScreenshotViewerImage").Source is not null &&
                    retry.Visibility == Visibility.Collapsed && !retry.IsEnabled,
                    "successful retry displays the image and removes its failure action");
                var image = NativeUi.ByName<Image>(root, "ScreenshotViewerImage");
                Check(image.Source is BitmapImage bitmap && bitmap.PixelWidth == 1280 && bitmap.PixelHeight == 640 && bitmap.DecodePixelWidth == 0,
                    "retry retains full original pixels rather than the bounded result-dialog thumbnail");
                NativeUi.Invoke(NativeUi.ByName<Button>(root, "ScreenshotViewerActual"));
                await WaitAsync(() => Math.Abs(NativeUi.ByName<ScrollViewer>(root, "ScreenshotViewerViewport").ZoomFactor - 1) < 0.001,
                    "100-percent mode still works after a recovered read");
                Check(Math.Abs(image.Width * root.XamlRoot.RasterizationScale - 1280) < 0.1 && api.Reads.Count == 2,
                    "recovered 100-percent layout preserves physical pixel mapping without another API read");
                NativeUi.Invoke(NativeUi.ByName<Button>(root, "ScreenshotViewerClose"));
                await shown;
                Check(!retry.IsEnabled && image.Source is null, "closing a recovered viewer disables retry and releases its original pixels");
            }
            _window.Activate();
            using var cancellation = new CancellationTokenSource();
            using var cancelled = new ScreenshotViewerWindow(api, source, cancellation.Token);
            var pending = cancelled.ShowAsync();
            await WaitAsync(() => api.Reads.Count == 3 && ((Grid)cancelled.Content).XamlRoot is not null,
                "a second retry viewer owns its distinct initial archive request");
            var cancelledRoot = (Grid)cancelled.Content;
            var cancelledRetry = NativeUi.ByName<Button>(cancelledRoot, "ScreenshotViewerRetry");
            api.Reads[2].Completion.TrySetException(new IOException("Synthetic second read failure."));
            await WaitAsync(() => cancelledRetry.IsEnabled, "the second failure exposes its own retry action");
            NativeUi.Invoke(cancelledRetry);
            await WaitAsync(() => api.Reads.Count == 4 && !cancelledRetry.IsEnabled, "the second retry is pending before conversation cancellation");
            cancellation.Cancel();
            await WaitAsync(() => pending.IsCompleted, "conversation cancellation closes a viewer while its retry is reading");
            api.Reads[3].Completion.TrySetResult(new(JsonSerializer.SerializeToElement(new
                { content = new[] { new { type = "image", mimeType = "image/png", data = png } } })));
            await SettleAsync();
            Check(!cancelledRetry.IsEnabled && api.Reads[3].Token.IsCancellationRequested && api.Reads.Count == 4 &&
                NativeUi.Descendants<Image>(cancelledRoot).All(image => image.Source is null),
                "closed/cancelled retry cannot repopulate the image or expose another action after a late archive response");
            await pending;
        }
        finally
        {
            UiText.Initialize(language);
            _window.Activate();
        }
    }
}
