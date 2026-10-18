using System.Text.Json;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using PathShape = Microsoft.UI.Xaml.Shapes.Path;
using Windows.Foundation;

namespace KYNXA_Desktop;

public partial class App
{
    private sealed record StartupHomeGeometry(Rect Composer, Rect Prompt, Rect Send, Rect Logo,
        double EditorHeight, double SurfaceHeight, bool FooterVisible);

    private async Task CheckInitialHomeAsync()
    {
        var geometry = new Dictionary<string, StartupHomeGeometry>();
        try
        {
            await WaitAsync(() => _gateway.CatalogReads == 1 && NativeUi.IsVisible(Element<Button>("ChatModeButton")) &&
                Element<ComposerSurface>("ComposerHost").ActualWidth > 0,
                "the first synthetic catalog request remains held while the real work homepage is visible");
            await SettleAsync();
            Check(!StartupProjectsReady && _gateway.Writes == 0,
                "startup checks run before the initial catalog can make projects ready");
            Check(!_shell.ViewModel.IsChatMode && Prompt.Text == string.Empty && ActiveId is null &&
                Prompt.PlaceholderText == UiText.Get("描述你想完成的工作..."),
                "the first work homepage uses its correct empty draft and work prompt before the catalog response");
            CheckStartupDisabledSend("the initial empty work homepage");
            var composer = Element<ComposerSurface>("ComposerHost");
            var workspace = Element<Button>("WorkspacePickerButton");
            Check(composer.IsFooterVisible && composer.FooterHeight == 44 && NativeUi.IsVisible(workspace) && !workspace.IsEnabled,
                "the initial work footer is visible at its final height while project selection remains unavailable");
            Check(Element<StackPanel>("MainContentHost").VerticalAlignment == VerticalAlignment.Center &&
                NativeUi.IsVisible(Element<Grid>("LogoHost")) && !NativeUi.IsVisible(Element<ConversationTranscript>("ConversationMessages")) &&
                _shell.ActiveMessages.Count == 0,
                "startup presents the centered empty homepage without any synthetic conversation becoming active");
            geometry["workBeforeCatalog"] = ReadStartupHomeGeometry();
            await CapturePresentedAsync("shell-startup-before-catalog.png");
            CheckStartupHomeGeometry(geometry["workBeforeCatalog"], ReadStartupHomeGeometry(),
                "the blocked work homepage remains at the same actual native position across rendered frames");

            // Choose a mode and draft before releasing the network; completion must respect both.
            // 在网络恢复前选择模式并填写草稿，初始化完成后应保留这两项用户选择。
            NativeUi.Invoke(Element<Button>("ChatModeButton"));
            await WaitAsync(() => _shell.ViewModel.IsChatMode, "the real Chat button works while the initial catalog is held");
            const string draft = "STARTUP_SYNTHETIC_CHAT_DRAFT";
            NativeUi.SetText(Prompt, draft);
            await SettleAsync();
            Check(Prompt.Text == draft && Prompt.PlaceholderText == UiText.Get("向 KYNXA 提问任何问题...") && ActiveId is null,
                "a draft typed during loading belongs to the chosen Chat mode without creating a conversation");
            Check(!composer.IsFooterVisible && !NativeUi.IsVisible(workspace),
                "changing to Chat removes the work footer immediately while the catalog is still held");
            CheckStartupDisabledSend("the nonempty Chat draft while projects are not ready");
            geometry["chatBeforeCatalog"] = ReadStartupHomeGeometry();

            _gateway.ReleaseInitialCatalog();
            await WaitAsync(() => StartupProjectsReady, "releasing the fixture catalog completes production project initialization");
            await SettleAsync();
            Check(_shell.ViewModel.IsChatMode && Prompt.Text == draft && ActiveId is null &&
                Prompt.PlaceholderText == UiText.Get("向 KYNXA 提问任何问题..."),
                "catalog completion preserves the user's chosen mode and exact loading-time draft");
            Check(Element<Button>("SendButton").IsEnabled && SendArrowUsesBrush("KynxaOnAccentBrush"),
                "the same nonempty draft becomes sendable after real project initialization without another edit");
            Check(!composer.IsFooterVisible && !NativeUi.IsVisible(workspace) && _shell.ActiveMessages.Count == 0,
                "catalog completion preserves the empty Chat homepage and its hidden work footer");
            geometry["chatAfterCatalog"] = ReadStartupHomeGeometry();
            CheckStartupHomeGeometry(geometry["chatBeforeCatalog"], geometry["chatAfterCatalog"],
                "catalog completion does not move or resize the unchanged Chat homepage");
            await CapturePresentedAsync("shell-startup-chat-after-catalog.png");

            NativeUi.SetText(Prompt, string.Empty);
            NativeUi.Invoke(Element<Button>("WorkModeButton"));
            await WaitAsync(() => !_shell.ViewModel.IsChatMode && Prompt.Text == string.Empty,
                "startup checks restore the original empty Work mode for the remaining suite");
            await SettleAsync();
            CheckStartupDisabledSend("the restored empty Work homepage");
            await WaitAsync(() => _gateway.ActivationRequests >= 1,
                "the initial temporary retrieval scope receives its background activation hint");
            var restoredFooter = new
            {
                FooterVisible = composer.IsFooterVisible,
                WorkspaceVisible = NativeUi.IsVisible(workspace),
                WorkspaceEnabled = workspace.IsEnabled,
                ActiveConversationId = ActiveId,
                PromptPlaceholder = Prompt.PlaceholderText,
                ExpectedPlaceholder = UiText.Get("描述你想完成的工作..."),
                _gateway.CatalogReads,
                _gateway.Writes,
                _gateway.ActivationRequests,
                FixtureFailure = _gateway.Failure?.ToString()
            };
            // Record every conjunct before failing so fixture mismatches are distinguishable from production regressions.
            // 失败前记录每项真实条件，用于区分模拟接口差异与产品回归，不放宽就绪、无写入或无新会话约束。
            string restoredFooterJson = JsonSerializer.Serialize(restoredFooter);
            File.WriteAllText(Path.Combine(_directory, "shell-startup-restored-footer.json"), restoredFooterJson);
            Check(composer.IsFooterVisible && NativeUi.IsVisible(workspace) && workspace.IsEnabled && ActiveId is null &&
                Prompt.PlaceholderText == UiText.Get("描述你想完成的工作...") && _gateway.CatalogReads == 1 && _gateway.Writes == 0,
                "the restored work footer becomes available after one catalog read without conversation/catalog writes or a new conversation: " + restoredFooterJson);
            Check(_gateway.ActivationRequests == 1 && _gateway.Failure is null && ActiveId is null,
                "unchanged startup scope activates retrieval once without creating a conversation");
            geometry["workAfterCatalog"] = ReadStartupHomeGeometry();
            CheckStartupHomeGeometry(geometry["workBeforeCatalog"], geometry["workAfterCatalog"],
                "the restored work homepage matches its native geometry from before the catalog response");
            File.WriteAllText(Path.Combine(_directory, "shell-startup-geometry.json"),
                JsonSerializer.Serialize(geometry, new JsonSerializerOptions { WriteIndented = true }));
        }
        finally
        {
            // A failed assertion must not leave the owned HTTP response blocked during disposal.
            // 断言失败时也释放夹具自有 HTTP 请求，防止退出时仍然等待门闩。
            _gateway.ReleaseInitialCatalog();
        }
    }

    private bool StartupProjectsReady => (bool)typeof(KYNXA_Desktop.Views.ShellPage)
        .GetField("_projectsReady", PrivateInstance)!.GetValue(_shell)!;

    private void CheckStartupDisabledSend(string description)
    {
        var send = Element<Button>("SendButton");
        var toolbar = Element<Grid>("ComposerToolbar");
        Rect sendBounds = send.TransformToVisual(toolbar).TransformBounds(new Rect(0, 0, send.ActualWidth, send.ActualHeight));
        // Equal button dimensions alone do not detect ancestor layout clipping of the circular caps.
        // 按钮宽高相等不能证明圆弧完整；同时核对按钮边界落在工具栏可见布局范围内。
        Check(Math.Abs(send.ActualWidth - send.ActualHeight) < .1 && sendBounds.Top >= -.1 &&
            sendBounds.Bottom <= toolbar.ActualHeight + .1,
            description + $" keeps the complete circular Send inside its toolbar: button={send.ActualWidth:F2}x{send.ActualHeight:F2}, " +
            $"top={sendBounds.Top:F2}, bottom={sendBounds.Bottom:F2}, toolbarHeight={toolbar.ActualHeight:F2}");
        var background = PresentedBackground(send);
        Check(!send.IsEnabled && background is not null && background.Color == AppearanceService.ParseColor(AppearanceService.Current.Disabled) &&
            SendArrowUsesBrush("KynxaDisabledTextBrush") && Element<PathShape>("SendArrow").Visibility == Visibility.Visible,
            description + " presents the real disabled button and readable themed arrow");
    }

    private StartupHomeGeometry ReadStartupHomeGeometry()
    {
        Rect Bounds(FrameworkElement control) => control.TransformToVisual(_shell)
            .TransformBounds(new Rect(0, 0, control.ActualWidth, control.ActualHeight));
        var composer = Element<ComposerSurface>("ComposerHost");
        return new(Bounds(composer), Bounds(Prompt), Bounds(Element<Button>("SendButton")), Bounds(Element<Grid>("LogoHost")),
            composer.EditorHeight, composer.SurfaceHeight, composer.IsFooterVisible);
    }

    private void CheckStartupHomeGeometry(StartupHomeGeometry before, StartupHomeGeometry after, string description)
    {
        static bool SameRect(Rect first, Rect second) =>
            Math.Abs(first.X - second.X) < 1 && Math.Abs(first.Y - second.Y) < 1 &&
            Math.Abs(first.Width - second.Width) < 1 && Math.Abs(first.Height - second.Height) < 1 &&
            first.Width > 0 && first.Height > 0 && second.Width > 0 && second.Height > 0;
        Check(SameRect(before.Composer, after.Composer) && SameRect(before.Prompt, after.Prompt) &&
            SameRect(before.Send, after.Send) && SameRect(before.Logo, after.Logo) &&
            before.EditorHeight == after.EditorHeight && before.SurfaceHeight == after.SurfaceHeight &&
            before.FooterVisible == after.FooterVisible, description);
    }
}
