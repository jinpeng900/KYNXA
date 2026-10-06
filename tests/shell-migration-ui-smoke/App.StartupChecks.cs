using System.Text.Json;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Imaging;
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
            Check(Element<Button>("SendButton").IsEnabled && Element<Image>("SendArrow").Source is SvgImageSource arrow &&
                arrow.UriSource.AbsolutePath.EndsWith("send-arrow.svg"),
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
            Check(composer.IsFooterVisible && NativeUi.IsVisible(workspace) && workspace.IsEnabled && ActiveId is null &&
                Prompt.PlaceholderText == UiText.Get("描述你想完成的工作...") && _gateway.CatalogReads == 1 && _gateway.Writes == 0,
                "the restored work footer becomes available after one read without gateway writes or a new conversation");
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
        var background = PresentedBackground(send);
        Check(!send.IsEnabled && background is not null && background.Color.R == 0xE2 &&
            background.Color.G == 0xE4 && background.Color.B == 0xE8 &&
            Element<Image>("SendArrow").Source is SvgImageSource arrow &&
            arrow.UriSource.AbsolutePath.EndsWith("send-arrow-disabled.svg"),
            description + " presents the real grey disabled button and readable disabled SVG");
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
