using System.Text.Json;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;

namespace KYNXA_Desktop;

public partial class App
{
    private sealed record AppearanceObservation(string Accent, string Soft, string Main, string Bubble,
        bool SameArticle, bool SameBody, bool SameSelection, string SelectedText, double ScrollTop,
        double OriginalScrollTop, double ScrollHeight, double Viewport);

    private static string CssRgb(string hex)
    {
        var color = AppearanceService.ParseColor(hex);
        return $"rgb({color.R}, {color.G}, {color.B})";
    }

    private static double NativeSelectedTextContrast(Windows.UI.Color background)
    {
        // WinUI uses white selected text in editable controls; measure that actual foreground pair.
        // WinUI 原生编辑控件的选中文字为白色，按这一真实前景与背景组合计算对比度。
        static double Linear(byte channel)
        {
            double value = channel / 255d;
            return value <= 0.04045 ? value / 12.92 : Math.Pow((value + 0.055) / 1.055, 2.4);
        }
        double luminance = 0.2126 * Linear(background.R) + 0.7152 * Linear(background.G) + 0.0722 * Linear(background.B);
        return 1.05 / (luminance + 0.05);
    }

    private async Task<AppearanceObservation> ObserveAppearanceAsync(ConversationTranscript transcript) =>
        JsonSerializer.Deserialize<AppearanceObservation>(await transcript.Browser.ExecuteScriptAsync("""
            (() => {
              const saved = window.__appearanceFixture;
              const root = getComputedStyle(document.documentElement);
              const selection = getSelection();
              const scroll = document.scrollingElement;
              return {
                Accent: root.getPropertyValue('--appearance-accent').trim().toUpperCase(),
                Soft: root.getPropertyValue('--appearance-soft').trim().toUpperCase(),
                Main: getComputedStyle(document.documentElement).backgroundColor,
                Bubble: getComputedStyle(document.querySelector('.user .message-content')).backgroundColor,
                SameArticle: saved.article === document.querySelector(saved.selector),
                SameBody: saved.body === document.querySelector(saved.selector + ' .message-content'),
                SameSelection: selection.anchorNode === saved.anchorNode && selection.focusNode === saved.focusNode &&
                  selection.anchorOffset === saved.anchorOffset && selection.focusOffset === saved.focusOffset,
                SelectedText: selection.toString(), ScrollTop: scroll.scrollTop,
                OriginalScrollTop: saved.scrollTop, ScrollHeight: scroll.scrollHeight, Viewport: scroll.clientHeight
              };
            })()
            """)) ?? throw new InvalidOperationException("The production transcript returned no appearance observation.");

    private async Task WaitForAppearanceAsync(ConversationTranscript transcript, AppearancePalette palette)
    {
        DateTime deadline = DateTime.UtcNow.AddSeconds(10);
        while ((await ObserveAppearanceAsync(transcript)).Accent != palette.Accent.ToUpperInvariant())
        {
            if (DateTime.UtcNow >= deadline) throw new TimeoutException("The production WebView did not apply " + palette.Id);
            await Task.Delay(30);
        }
        await SettleAsync();
    }

    private async Task CheckAppearanceAsync()
    {
        string originalPaletteId = AppearanceService.Current.Id;
        string originalLanguage = UiText.Language;
        Guid? originalChatId = ActiveId;
        string originalDraft = Prompt.Text;
        Window? settings = null;
        var transcript = Element<ConversationTranscript>("ConversationMessages");
        var observations = new List<object>();
        try
        {
            Check(new LayoutStateService().Load().AppearancePaletteId == AppearanceService.Current.Id &&
                AppearanceService.Current.Id == AppearanceService.DefaultPaletteId,
                "the initialized production Shell applies the palette loaded from its isolated startup preference");
            string layoutPath = Path.Combine(DesktopDirectory, "layout.json");
            string initialPreference = File.ReadAllText(layoutPath);
            try
            {
                File.WriteAllText(layoutPath, "{\"LayoutVersion\":4,\"SidebarWidth\":318}");
                Check(new LayoutStateService().Load().AppearancePaletteId == AppearanceService.DefaultPaletteId,
                    "older preferences without a palette field load the default light palette");
                var unknownPreference = JsonSerializer.Deserialize<LayoutState>(initialPreference)!;
                unknownPreference.AppearancePaletteId = "unknown-future-palette";
                File.WriteAllText(layoutPath, JsonSerializer.Serialize(unknownPreference));
                Check(new LayoutStateService().Load().AppearancePaletteId == AppearanceService.DefaultPaletteId &&
                    AppearanceService.Current.Id == originalPaletteId,
                    "unknown stored palette values fall back without changing the active appearance");
            }
            finally { File.WriteAllText(layoutPath, initialPreference); }
            await SearchAndSelectAsync(_gateway.StandaloneChatId, "Chat fixture");
            const string draft = "APPEARANCE_FIXTURE_UNSENT_DRAFT 中文 $x^2$";
            NativeUi.SetText(Prompt, draft);
            Guid conversationId = ActiveId!.Value;
            var originalRows = _shell.ActiveMessages.ToArray();
            string originalMessages = JsonSerializer.Serialize(originalRows.Select(row => row.Message));
            string originalCatalog = JsonSerializer.Serialize(_gateway.Catalog);
            var ownedMessage = new ChatMessageState { Role = "assistant", Content = string.Join("\n\n",
                Enumerable.Range(1, 120).Select(index => $"APPEARANCE_SCROLL_{index:000} 仅供配色切换验收的虚构正文，保留选区与滚动位置。")) };
            // This long row belongs only to the fixture's view, never to the formal message collection.
            // 此长消息仅由夹具视图持有，不加入正式消息集合。
            transcript.ShowConversation(conversationId, [.. originalRows, new ConversationMessageViewModel(conversationId, ownedMessage)], openAtBottom: true);
            string selector = $"article[data-message-id=\"{ownedMessage.Id}\"]";
            DateTime renderingDeadline = DateTime.UtcNow.AddSeconds(15);
            while (!JsonSerializer.Deserialize<bool>(await transcript.Browser.ExecuteScriptAsync(
                "!!document.querySelector(" + JsonSerializer.Serialize(selector) + ")")))
            {
                if (DateTime.UtcNow >= renderingDeadline) throw new TimeoutException("The owned appearance transcript row did not render.");
                await Task.Delay(30);
            }
            await SettleAsync();
            Call("StorageSettings_Click", _shell, new RoutedEventArgs());
            settings = ShellField<Window>("_storageSettingsWindow");
            var root = (FrameworkElement)settings.Content;
            await WaitAsync(() => root.IsLoaded && root.XamlRoot is not null,
                "the production appearance settings window is loaded");
            await SettleAsync();
            var paletteGrid = NativeUi.ById<Grid>(root, "AppearancePaletteGrid");
            var buttons = AppearanceService.Palettes.Select(palette =>
                NativeUi.ById<Button>(root, "AppearancePalette_" + palette.Id)).ToArray();
            Check(buttons.Length == 5 && buttons.All(button => button.IsEnabled),
                "the real settings page offers exactly the five enabled light palette cards");

            bool prepared = JsonSerializer.Deserialize<bool>(await transcript.Browser.ExecuteScriptAsync($$"""
                (() => {
                  const selector = {{JsonSerializer.Serialize(selector)}};
                  const article = document.querySelector(selector);
                  const body = article.querySelector('.message-content');
                  const walker = document.createTreeWalker(body, NodeFilter.SHOW_TEXT);
                  let node;
                  while ((node = walker.nextNode()) && node.textContent.trim().length < 20) {}
                  if (!node) return false;
                  const range = document.createRange(); range.setStart(node, 0); range.setEnd(node, 20);
                  const selection = getSelection(); selection.removeAllRanges(); selection.addRange(range);
                  const scroll = document.scrollingElement; scroll.scrollTop = 240;
                  window.__appearanceFixture = { selector, article, body, anchorNode: selection.anchorNode,
                    focusNode: selection.focusNode, anchorOffset: selection.anchorOffset,
                    focusOffset: selection.focusOffset, selectedText: selection.toString(), scrollTop: scroll.scrollTop };
                  return scroll.scrollTop > 20 && scroll.scrollHeight > scroll.clientHeight * 3 && selection.toString().length === 20;
                })()
                """));
            Check(prepared, "the real transcript has a long scrollable document and an exact selected text range before palette changes");
            var firstObservation = await ObserveAppearanceAsync(transcript);
            var sendBrush = AppearanceService.GetBrush("KynxaSendBrush");
            var sidebarBrush = AppearanceService.GetBrush("KynxaSidebarBrush");
            var selectionBrush = AppearanceService.GetBrush("KynxaSelectionBrush");

            foreach (var palette in AppearanceService.Palettes)
            {
                var button = NativeUi.ById<Button>(root, "AppearancePalette_" + palette.Id);
                button.StartBringIntoView();
                await SettleAsync();
                NativeUi.Invoke(button);
                await WaitForAppearanceAsync(transcript, palette);
                var observation = await ObserveAppearanceAsync(transcript);
                var promptSelectionColor = Prompt.SelectionHighlightColor?.Color;
                double selectedTextContrast = promptSelectionColor is { } selectedBackground
                    ? NativeSelectedTextContrast(selectedBackground) : 0;
                observations.Add(new { Palette = palette.Id, Observation = observation,
                    NativeSelectionColor = promptSelectionColor is { } observedColor ?
                        $"#{observedColor.R:X2}{observedColor.G:X2}{observedColor.B:X2}" : null,
                    NativeSelectedWhiteTextContrast = selectedTextContrast });
                Check(AppearanceService.Current.Id == palette.Id && new LayoutStateService().Load().AppearancePaletteId == palette.Id &&
                    ShellField<LayoutState>("_layout").AppearancePaletteId == palette.Id,
                    $"clicking the actual {palette.Id} card applies and reloads its locally saved preference");
                Check(ReferenceEquals(sendBrush, AppearanceService.GetBrush("KynxaSendBrush")) &&
                    ReferenceEquals(sidebarBrush, Element<Grid>("SidebarPane").Background) &&
                    ReferenceEquals(selectionBrush, Element<Border>("PrimaryModeSelectionPill").Background) &&
                    sendBrush.Color == AppearanceService.ParseColor(palette.Accent) &&
                    sidebarBrush.Color == AppearanceService.ParseColor(palette.Sidebar) &&
                    selectionBrush.Color == AppearanceService.ParseColor(palette.Soft),
                    $"{palette.Id} updates the existing native send, sidebar and selected mode brushes in place");
                var sendBackground = PresentedBackground(Element<Button>("SendButton"));
                Check(Element<Button>("SendButton").IsEnabled && sendBackground is not null &&
                    new[] { palette.Accent, palette.AccentHover, palette.AccentPressed }.Any(hex => sendBackground.Color == AppearanceService.ParseColor(hex)),
                    $"{palette.Id} reaches the rendered enabled native Send button");
                Check(promptSelectionColor is { A: 255 } && promptSelectionColor.Value == AppearanceService.ParseColor(palette.Accent) &&
                    selectedTextContrast >= 4.5,
                    $"{palette.Id} gives actual native prompt selection an opaque accent background with white-text contrast {selectedTextContrast:F2}:1 (at least 4.5:1)");
                Check(observation.Soft == palette.Soft.ToUpperInvariant() && observation.Main == CssRgb(palette.Main) &&
                    observation.Bubble == CssRgb(palette.Soft) && transcript.Browser.DefaultBackgroundColor == AppearanceService.ParseColor(palette.Main),
                    $"{palette.Id} synchronizes the real WebView canvas and user message bubble with native colors");
                Check(observation.SameArticle && observation.SameBody && observation.SameSelection &&
                    observation.SelectedText == firstObservation.SelectedText && Math.Abs(observation.ScrollTop - observation.OriginalScrollTop) <= 1 &&
                    Prompt.Text == draft && ActiveId == conversationId && _shell.ActiveMessages.SequenceEqual(originalRows) &&
                    JsonSerializer.Serialize(originalRows.Select(row => row.Message)) == originalMessages &&
                    JsonSerializer.Serialize(_gateway.Catalog) == originalCatalog && _gateway.Writes == 0,
                    $"{palette.Id} preserves exact draft, message objects, body DOM, text selection and scroll without gateway writes");
                Check(AutomationProperties.GetItemStatus(button) == UiText.Get("已选中") && buttons.Count(candidate =>
                    AutomationProperties.GetItemStatus(candidate) == UiText.Get("已选中")) == 1,
                    $"{palette.Id} has one accessible selected card status");
            }

            var status = NativeUi.ById<InfoBar>(root, "AppearanceSettingsStatus");
            string selectedId = AppearanceService.Current.Id;
            string savedPreference = File.ReadAllText(Path.Combine(DesktopDirectory, "layout.json"));
            try
            {
                StoragePaths.IsMigrating = true;
                NativeUi.Invoke(buttons[0]);
                Check(AppearanceService.Current.Id == selectedId && status.IsOpen &&
                    File.ReadAllText(Path.Combine(DesktopDirectory, "layout.json")) == savedPreference,
                    "a palette click during migration leaves the displayed palette and saved preference intact with a visible error");
            }
            finally { StoragePaths.IsMigrating = false; }

            string blockedTemporaryPath = Path.Combine(DesktopDirectory, "layout.json.tmp");
            Directory.CreateDirectory(blockedTemporaryPath);
            try
            {
                NativeUi.Invoke(buttons[0]);
                Check(AppearanceService.Current.Id == selectedId && ShellField<LayoutState>("_layout").AppearancePaletteId == selectedId &&
                    File.ReadAllText(Path.Combine(DesktopDirectory, "layout.json")) == savedPreference && status.IsOpen &&
                    status.Message == UiText.Get("配色未能保存，已保留原来的外观，请稍后重试。"),
                    "an actual temporary-file save failure rolls back the requested palette and shows its localized error");
            }
            finally { Directory.Delete(blockedTemporaryPath); }

            UiText.Initialize("en");
            double scale = root.XamlRoot.RasterizationScale;
            settings.AppWindow.Resize(new Windows.Graphics.SizeInt32((int)(400 * scale), (int)(540 * scale)));
            await SettleAsync();
            Check(paletteGrid.ColumnDefinitions.Count <= 2 && buttons.All(button =>
            {
                Rect bounds = button.TransformToVisual(paletteGrid).TransformBounds(new Rect(0, 0, button.ActualWidth, button.ActualHeight));
                return bounds.Width > 100 && bounds.Left >= -1 && bounds.Right <= paletteGrid.ActualWidth + 1 &&
                    AutomationProperties.GetName(button) == UiText.Get(AppearanceService.Palettes.Single(palette => "AppearancePalette_" + palette.Id ==
                        AutomationProperties.GetAutomationId(button)).NameKey);
            }), "400 DIP English settings lays out all five named palette cards within its responsive grid");
            buttons[^1].StartBringIntoView();
            await SettleAsync();
            await NativeWindowCapture.CaptureAsync(settings, Path.Combine(_directory, "shell-appearance-settings-400-en.png"));
            var reset = NativeUi.ById<Button>(root, "AppearanceResetButton");
            reset.StartBringIntoView();
            await SettleAsync();
            NativeUi.Invoke(reset);
            await WaitForAppearanceAsync(transcript, AppearanceService.Palettes.Single(palette => palette.Id == AppearanceService.DefaultPaletteId));
            Check(AppearanceService.Current.Id == AppearanceService.DefaultPaletteId && !reset.IsEnabled && !status.IsOpen &&
                new LayoutStateService().Load().AppearancePaletteId == AppearanceService.DefaultPaletteId && Prompt.Text == draft && ActiveId == conversationId,
                "the actual Restore default action saves Mist blue, clears the error and preserves the current conversation");
            UiText.Initialize("zh-CN");
            settings.AppWindow.Resize(new Windows.Graphics.SizeInt32((int)(660 * scale), (int)(650 * scale)));
            await SettleAsync();
            reset.StartBringIntoView();
            await SettleAsync();
            await NativeWindowCapture.CaptureAsync(settings, Path.Combine(_directory, "shell-appearance-settings-wide.png"));
            settings.Close();
            settings = null;
            Window.Activate();
            Prompt.Focus(FocusState.Programmatic);
            await SettleAsync();
            var editorSurface = (Border)Element<ComposerSurface>("ComposerHost").FindName("EditorSurface");
            Check(ReferenceEquals(editorSurface.BorderBrush, AppearanceService.GetBrush("KynxaFocusBrush")),
                "focusing the real prompt shows the palette accent on the enclosing composer border");
            // Native editable text has its own selected-text foreground, separate from WebView CSS.
            // 原生输入框的选中文字前景独立于 WebView CSS，须实际选中并留图核验。
            Prompt.SelectAll();
            await SettleAsync();
            Check(Prompt.SelectionStart == 0 && Prompt.SelectionLength == draft.Length && Prompt.SelectedText == draft,
                "the focused native prompt retains an exact full-text selection for palette contrast verification");
            var nativeSelectionBrush = Prompt.SelectionHighlightColor;
            File.WriteAllText(Path.Combine(_directory, "shell-appearance-native-selection.json"),
                JsonSerializer.Serialize(new
                {
                    Palette = AppearanceService.Current.Id,
                    SelectionStart = Prompt.SelectionStart,
                    SelectionLength = Prompt.SelectionLength,
                    SelectionColor = nativeSelectionBrush is null ? null :
                        $"#{nativeSelectionBrush.Color.R:X2}{nativeSelectionBrush.Color.G:X2}{nativeSelectionBrush.Color.B:X2}",
                    NativeForeground = Prompt.Foreground is SolidColorBrush foreground ?
                        $"#{foreground.Color.R:X2}{foreground.Color.G:X2}{foreground.Color.B:X2}" : null
                }, new JsonSerializerOptions { WriteIndented = true }));
            await NativeWindowCapture.CaptureAsync(Window, Path.Combine(_directory, "shell-appearance-native-selection.png"));
            Prompt.Select(Prompt.Text.Length, 0);
            Element<Button>("HistorySearchButton").Focus(FocusState.Programmatic);
            await SettleAsync();
            Check(ReferenceEquals(editorSurface.BorderBrush, AppearanceService.GetBrush("KynxaComposerBorderBrush")) &&
                Prompt.Text == draft && ActiveId == conversationId,
                "leaving the composer restores its quiet border without changing the draft or conversation");
            await transcript.Browser.ExecuteScriptAsync("getSelection().removeAllRanges()");
            transcript.ShowConversation(ActiveId, originalRows, openAtBottom: false);
            await SettleAsync();
            await CapturePresentedAsync("shell-appearance-mist-blue-wide.png");
            File.WriteAllText(Path.Combine(_directory, "shell-appearance-observations.json"),
                JsonSerializer.Serialize(observations, new JsonSerializerOptions { WriteIndented = true }));
        }
        finally
        {
            StoragePaths.IsMigrating = false;
            settings?.Close();
            UiText.Initialize(originalLanguage);
            var layout = ShellField<LayoutState>("_layout");
            layout.AppearancePaletteId = originalPaletteId;
            new LayoutStateService().Save(layout);
            AppearanceService.Apply(originalPaletteId);
            await transcript.Browser.ExecuteScriptAsync("delete window.__appearanceFixture; getSelection().removeAllRanges()");
            transcript.ShowConversation(ActiveId, _shell.ActiveMessages.ToArray(), openAtBottom: false);
            if (originalChatId is { } chatId && chatId != ActiveId)
                await SearchAndSelectAsync(chatId, chatId == _gateway.WorkChatId ? "Work fixture" : "Chat fixture");
            NativeUi.SetText(Prompt, originalDraft);
            ResizeInDips(1440, 900);
            await SettleAsync();
        }
    }
}
