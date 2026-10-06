using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.ApplicationModel.DataTransfer;
using Windows.Foundation;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;

namespace AgentUiSmoke;

public partial class App
{
    // Normalize only the TextBox display contract; clipboard assertions still compare the original text exactly.
    // 仅归一化 TextBox 的显示换行；剪贴板断言仍与原文严格比较。
    private static string NormalizeDialogDisplayText(string text) => text.Replace("\r\n", "\n").Replace('\r', '\n');

    private bool DialogControlFitsWidth(FrameworkElement control, ScrollViewer scroll)
    {
        var bounds = control.TransformToVisual(scroll).TransformBounds(new Rect(0, 0, control.ActualWidth, control.ActualHeight));
        return control.ActualWidth > 0 && bounds.Left >= -1 && bounds.Right <= scroll.ViewportWidth + 1;
    }

    private void ResizeDialogFixture(int width, int height)
    {
        double scale = _root.XamlRoot.RasterizationScale;
        _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32((int)Math.Ceiling(width * scale), (int)Math.Ceiling(height * scale)));
    }

    private async Task CheckDialogPolishAsync()
    {
        string language = UiText.Language;
        var originalSize = _window!.AppWindow.Size;
        var originalDelayed = _api.DelayedResult;
        string? originalImagePath = _api.ScreenshotImagePath;
        ContentDialog? openDialog = null;
        int saves = _api.Saves, connections = _api.Connections;
        try
        {
            UiText.Initialize("en");
            ResizeDialogFixture(480, 540);
            await SettleAsync();
            string longToolName = "mcp.fixture." + string.Join(".", Enumerable.Repeat("long_provider_and_tool_name", 8));
            string longWorkspace = Path.Combine(_directory, string.Concat(Enumerable.Repeat("long-workspace-segment-", 8)), "中文项目");
            var approvalTool = new ToolActivity("dialog-polish-approval", longToolName, System.Text.Json.JsonSerializer.SerializeToElement(new
                { path = longWorkspace, literal = "No fixture command is executed." }), "approval-required", "Review only this synthetic requested operation.",
                ApprovalId: Guid.NewGuid(), WorkspaceRoot: longWorkspace, OutsideWorkspace: false, Sandbox: "fixture-only");
            var approval = ToolApprovalDialog.Create(_root.XamlRoot, approvalTool);
            openDialog = approval;
            var decision = approval.ShowAsync();
            await WaitAsync(() => NativeUi.OpenDialog(_root) == approval, "long-name approval opens in the narrow native fixture");
            await SettleAsync();
            var approvalScroll = NativeUi.ByName<ScrollViewer>(approval, "ToolApprovalContentScroll");
            var approvalName = NativeUi.ByName<TextBlock>(approval, "ToolApprovalName");
            var workspace = NativeUi.ByName<TextBlock>(approval, "ToolApprovalWorkspace");
            Check(approvalName.Text == longToolName && approvalName.ActualHeight > 24 && DialogControlFitsWidth(approvalName, approvalScroll) &&
                workspace.Text == longWorkspace && workspace.IsTextSelectionEnabled && DialogControlFitsWidth(workspace, approvalScroll),
                "long literal tool names and workspace paths wrap within the approval content instead of being lost or widening the dialog");
            Check(approvalScroll.MaxHeight <= _root.XamlRoot.Size.Height - 190 && approvalScroll.ScrollableHeight > 0 &&
                approval.DefaultButton == ContentDialogButton.None,
                "short approval content scrolls while retaining an explicit decision with no default approval");
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "approval-narrow-long-name-en.png"));
            NativeUi.InvokeDialogButton(approval, primary: false);
            Check(await decision == ContentDialogResult.None, "the reachable close button still denies the synthetic approval");
            openDialog = null;
            await Task.Delay(250);

            string firstText = "已加载的原文 😀\n  保留缩进及 TeX：$x^2$\n";
            string secondText = "第二段结束。";
            int pages = _api.ResultPages;
            var firstPage = new TaskCompletionSource<ToolResultPage>(TaskCreationOptions.RunContinuationsAsynchronously);
            _api.DelayedResult = firstPage;
            var resultTool = new ToolActivity("dialog-polish-result", longToolName, null, "completed", "Synthetic result",
                ResultRef: _api.ResultReference);
            using (var viewer = new ToolResultDialog(_root.XamlRoot, _api, Guid.NewGuid(), resultTool))
            {
                openDialog = viewer.Dialog;
                var shown = viewer.ShowAsync();
                await WaitAsync(() => _api.ResultPages == pages + 1 && NativeUi.OpenDialog(_root) == viewer.Dialog,
                    "result dialog waits on one isolated first-page request");
                var copy = NativeUi.ByName<Button>(viewer.Dialog, "ToolResultCopy");
                var text = NativeUi.ByName<TextBox>(viewer.Dialog, "ToolResultText");
                var scroll = NativeUi.ByName<ScrollViewer>(viewer.Dialog, "ToolResultContentScroll");
                var actions = NativeUi.ByName<StackPanel>(viewer.Dialog, "ToolResultActions");
                Check(!copy.IsEnabled && text.Text.Length == 0, "copy is disabled while the first result page is empty and still loading");
                firstPage.SetResult(new(_api.ResultReference.Id, firstText, firstText.Length + secondText.Length, 0, firstText.Length, true, _api.ResultReference));
                await WaitAsync(() => copy.IsEnabled && NormalizeDialogDisplayText(text.Text) == firstText, "copy becomes enabled only after actual text arrives");
                await SettleAsync();
                var name = NativeUi.ByName<TextBlock>(viewer.Dialog, "ToolResultName");
                Check(name.Text == longToolName && name.ActualHeight > 24 && DialogControlFitsWidth(name, scroll) &&
                    actions.Orientation == Orientation.Vertical && actions.Children.OfType<Button>().All(button => DialogControlFitsWidth(button, scroll)),
                    "English result actions stack within a narrow dialog and the complete tool name remains readable");
                copy.StartBringIntoView();
                await SettleAsync();
                NativeUi.Invoke(copy);
                await WaitAsync(() => NativeUi.ByName<TextBlock>(viewer.Dialog, "ToolResultCopyStatus").Text == UiText.Get("已复制"),
                    "copy feedback acknowledges the actual native clipboard operation");
                Check(await Clipboard.GetContent().GetTextAsync() == firstText &&
                    AutomationProperties.GetLiveSetting(NativeUi.ByName<TextBlock>(viewer.Dialog, "ToolResultCopyStatus")) == AutomationLiveSetting.Polite,
                    "clipboard preserves Unicode, raw formula source and indentation and provides polite accessible success feedback");
                UiText.Initialize("zh-CN");
                await SettleAsync();
                Check(NativeUi.ByName<TextBlock>(viewer.Dialog, "ToolResultCopyStatus").Text == "已复制" && NormalizeDialogDisplayText(text.Text) == firstText,
                    "live language switching localizes copy feedback without changing the loaded result");
                UiText.Initialize("en");
                var secondPage = new TaskCompletionSource<ToolResultPage>(TaskCreationOptions.RunContinuationsAsynchronously);
                _api.DelayedResult = secondPage;
                NativeUi.Invoke(NativeUi.ByName<Button>(viewer.Dialog, "ToolResultMore"));
                await WaitAsync(() => _api.ResultPages == pages + 2 && !copy.IsEnabled, "copy is disabled during the next-page request");
                secondPage.SetResult(new(_api.ResultReference.Id, secondText, firstText.Length + secondText.Length, firstText.Length,
                    firstText.Length + secondText.Length, false, _api.ResultReference));
                await WaitAsync(() => copy.IsEnabled && NormalizeDialogDisplayText(text.Text) == firstText + secondText, "appended result text restores copy availability");
                Check(NativeUi.ByName<TextBlock>(viewer.Dialog, "ToolResultCopyStatus").Visibility == Visibility.Collapsed,
                    "a new load clears stale copy success feedback");
                _api.DelayedResult = null;

                _api.ScreenshotImagePath = await WriteDialogFixtureImageAsync();
                int reads = _api.ResultReads;
                NativeUi.Invoke(NativeUi.ByName<Button>(viewer.Dialog, "ToolResultMedia"));
                var resources = NativeUi.ByName<StackPanel>(viewer.Dialog, "ToolResultResources");
                await WaitAsync(() => _api.ResultReads == reads + 1 && resources.Children.OfType<Image>().Any(image => image.Source is not null) && copy.IsEnabled,
                    "only the explicit media action reads and displays the isolated image archive");
                var bitmap = (BitmapImage)resources.Children.OfType<Image>().Single().Source;
                Check(bitmap.DecodePixelType == DecodePixelType.Physical && bitmap.DecodePixelWidth == 1024 && bitmap.DecodePixelHeight == 512,
                    "result-dialog thumbnails decode bounded physical pixels instead of retaining a full 3000 by 1500 bitmap");
                ResizeDialogFixture(940, 720);
                await SettleAsync();
                Check(actions.Children.OfType<Button>().Where(button => button.Visibility == Visibility.Visible)
                    .All(button => DialogControlFitsWidth(button, scroll)),
                    "widening the open result dialog keeps every visible action within its actual content width");
                ResizeDialogFixture(480, 540);
                await WaitAsync(() => actions.Orientation == Orientation.Vertical,
                    "narrowing the open result dialog keeps the same text and resources with accessible stacked actions");
                copy.StartBringIntoView();
                await SettleAsync();
                await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "result-narrow-actions-en.png"));
                NativeUi.InvokeDialogButton(viewer.Dialog, primary: false);
                await shown;
                openDialog = null;
            }
            await Task.Delay(250);

            var emptyPage = new TaskCompletionSource<ToolResultPage>(TaskCreationOptions.RunContinuationsAsynchronously);
            _api.DelayedResult = emptyPage;
            using (var empty = new ToolResultDialog(_root.XamlRoot, _api, Guid.NewGuid(), resultTool))
            {
                openDialog = empty.Dialog;
                var shown = empty.ShowAsync();
                await WaitAsync(() => NativeUi.OpenDialog(_root) == empty.Dialog, "a separate empty-result dialog opens");
                emptyPage.SetResult(new(_api.ResultReference.Id, string.Empty, 0, 0, 0, false, _api.ResultReference));
                await WaitAsync(() => NativeUi.ByName<Button>(empty.Dialog, "ToolResultMore").Visibility == Visibility.Collapsed,
                    "an authoritative empty result completes loading");
                Check(!NativeUi.ByName<Button>(empty.Dialog, "ToolResultCopy").IsEnabled,
                    "empty successful content cannot misleadingly report a copy operation");
                NativeUi.InvokeDialogButton(empty.Dialog, primary: false);
                await shown;
                openDialog = null;
            }
            Check(_api.Saves == saves && _api.Connections == connections && _unhandled is null,
                "dialog polish neither saves configuration nor launches tools nor produces unhandled errors");
        }
        finally
        {
            openDialog?.Hide();
            _api.DelayedResult = originalDelayed;
            _api.ScreenshotImagePath = originalImagePath;
            UiText.Initialize(language);
            _window!.AppWindow.Resize(originalSize);
            await Task.Delay(250);
        }
    }

    private async Task<string> WriteDialogFixtureImageAsync()
    {
        const uint width = 3000, height = 1500;
        byte[] pixels = new byte[checked((int)(width * height * 4))];
        for (int index = 0; index < pixels.Length; index += 4)
        {
            pixels[index] = 230; pixels[index + 1] = 210; pixels[index + 2] = 190; pixels[index + 3] = 255;
        }
        using var stream = new InMemoryRandomAccessStream();
        var encoder = await BitmapEncoder.CreateAsync(BitmapEncoder.PngEncoderId, stream);
        encoder.SetPixelData(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Ignore, width, height, 96, 96, pixels);
        await encoder.FlushAsync(); stream.Seek(0);
        string path = Path.Combine(_directory, "dialog-synthetic-image.png");
        using var output = File.Create(path);
        await stream.AsStreamForRead().CopyToAsync(output);
        return path;
    }
}
