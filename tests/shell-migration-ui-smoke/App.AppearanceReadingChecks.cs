using System.Runtime.InteropServices;
using System.Text.Json;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;
using Windows.UI.ViewManagement;

namespace KYNXA_Desktop;

public partial class App
{
    private sealed record ReadingAppearanceObservation(string FontSize, string Text, string AccentText,
        string Error, string CodeComment, string ColorScheme, bool SameArticle, bool SameSelection,
        string SelectedText, string AnchorText, double AnchorTop, bool SameAnchor);

    private static void SelectAppearanceItem(ComboBox picker, object tag)
        => picker.SelectedItem = picker.Items.Cast<ComboBoxItem>().Single(item => Equals(item.Tag, tag));

    private static void SelectAppearanceItem(SegmentedSelector picker, string key) => picker.SelectedKey = key;

    private async Task<ReadingAppearanceObservation> ObserveReadingAppearanceAsync(ConversationTranscript transcript)
        => JsonSerializer.Deserialize<ReadingAppearanceObservation>(await transcript.Browser.ExecuteScriptAsync("""
            (() => {
              const fixture = window.__appearanceFixture;
              const saved = window.__appearanceReadingAnchor;
              const style = getComputedStyle(document.documentElement);
              const selection = getSelection();
              return {
                FontSize: style.fontSize, Text: style.getPropertyValue('--appearance-text').trim().toUpperCase(),
                AccentText: style.getPropertyValue('--appearance-accent-text').trim().toUpperCase(),
                Error: style.getPropertyValue('--appearance-error').trim().toUpperCase(),
                CodeComment: style.getPropertyValue('--code-comment').trim().toUpperCase(),
                ColorScheme: style.colorScheme,
                SameArticle: fixture.article === document.querySelector(fixture.selector),
                SameSelection: selection.anchorNode === fixture.anchorNode && selection.focusNode === fixture.focusNode &&
                  selection.anchorOffset === fixture.anchorOffset && selection.focusOffset === fixture.focusOffset,
                SelectedText: selection.toString(), AnchorText: saved?.node.textContent || '',
                AnchorTop: saved?.node.getBoundingClientRect().top ?? 0,
                SameAnchor: !!saved?.node.isConnected
              };
            })()
            """)) ?? throw new InvalidOperationException("Reading appearance observation was empty.");

    private async Task<JsonElement> ObserveAppearanceAnchorGeometryAsync(ConversationTranscript transcript)
        => JsonSerializer.Deserialize<JsonElement>(await transcript.Browser.ExecuteScriptAsync("""
            (() => {
              const describe = node => {
                const rect=node.getBoundingClientRect();
                return {Tag:node.tagName,Text:node.textContent.slice(0,160),Top:rect.top,Bottom:rect.bottom,Height:rect.height};
              };
              const candidates=[...document.querySelectorAll('article')]
                .filter(article=>{const rect=article.getBoundingClientRect();return rect.bottom>0&&rect.top<innerHeight;})
                .flatMap(article=>[...article.querySelectorAll('.message-body > *, .tool-activity')])
                .filter(node=>{const rect=node.getBoundingClientRect();return rect.height&&rect.bottom>0&&rect.top<innerHeight;});
              return {ScrollTop:scrollY,Viewport:innerHeight,ScrollHeight:document.documentElement.scrollHeight,
                FontSize:getComputedStyle(document.documentElement).fontSize,State:window.transcriptState(),
                Saved:window.__appearanceReadingAnchor?.node?describe(__appearanceReadingAnchor.node):null,
                Candidates:candidates.slice(0,12).map(describe)};
            })()
            """));

    private async Task CheckAppearanceReadingSettingsAsync(Window settings, FrameworkElement root, ConversationTranscript transcript)
    {
        string originalId = AppearanceService.Current.Id, originalMode = AppearanceService.Mode;
        double originalFontSize = AppearanceService.ChatFontSize;
        string? originalAccent = AppearanceService.CustomAccent;
        string originalLanguage = UiText.Language;
        string draft = Prompt.Text;
        Guid? conversationId = ActiveId;
        string formalMessages = JsonSerializer.Serialize(_shell.ActiveMessages.Select(row => row.Message));
        string layoutPath = Path.Combine(DesktopDirectory, "layout.json");
        NativeUi.Descendants<SettingsWorkspace>(root).Single().SelectPage("appearance");
        await SettleAsync();
        var modePicker = NativeUi.ById<SegmentedSelector>(root, "AppearanceModePicker");
        var fontPicker = NativeUi.ById<ComboBox>(root, "ChatFontSizePicker");
        var accentInput = NativeUi.ById<TextBox>(root, "AppearanceAccentInput");
        var applyAccent = NativeUi.ById<Button>(root, "AppearanceAccentApplyButton");
        var resetAccent = NativeUi.ById<Button>(root, "AppearanceAccentResetButton");
        var chooseColor = NativeUi.ById<Button>(root, "AppearanceColorPickerButton");
        var colorFlyout = chooseColor.Flyout as Flyout ?? throw new InvalidOperationException("The production color chooser has no flyout.");
        var colorPicker = colorFlyout.Content as ColorPicker ?? throw new InvalidOperationException("The production color chooser has no ColorPicker.");
        var reset = NativeUi.ById<Button>(root, "AppearanceResetButton");
        var status = NativeUi.ById<InfoBar>(root, "AppearanceSettingsStatus");
        var applicationAccentBrush = AppearanceService.GetBrush("KynxaAccentBrush");
        var nativeAccentBrush = AppearanceService.GetBrush("AccentFillColorDefaultBrush");
        var evidence = new List<object>();
        Check(NativeUi.Descendants<RadioButton>(modePicker).Select(item => item.Tag).SequenceEqual(new object[] { "light", "dark", "system" }) &&
            fontPicker.Items.Count == 19 && (double)((ComboBoxItem)fontPicker.Items[0]).Tag == 13 &&
            (double)((ComboBoxItem)fontPicker.Items[^1]).Tag == 22,
            "production settings exposes three theme modes and every half-pixel chat size from 13 to 22");
        Check(AppearanceService.NormalizeMode("unknown") == "light" && AppearanceService.NormalizeMode(null) == "light" &&
            AppearanceService.NormalizeChatFontSize(double.NaN) == 15 &&
            AppearanceService.NormalizeChatFontSize(double.PositiveInfinity) == 15 &&
            AppearanceService.NormalizeChatFontSize(-100) == 13 && AppearanceService.NormalizeChatFontSize(100) == 22 &&
            AppearanceService.NormalizeChatFontSize(15.3) == 15.5 &&
            AppearanceService.NormalizeAccent(" #abcDEF ") == "#ABCDEF" &&
            AppearanceService.NormalizeAccent("#GGGGGG") is null && AppearanceService.NormalizeAccent("#123") is null,
            "unknown modes, non-finite font sizes and malformed HEX values normalize without escaping their local preference bounds");

        string stored = File.ReadAllText(layoutPath);
        try
        {
            File.WriteAllText(layoutPath, "{\"LayoutVersion\":4,\"AppearanceMode\":\"future\",\"ChatFontSize\":100,\"AppearanceAccent\":\"invalid\"}");
            var normalized = new LayoutStateService().Load();
            Check(normalized.AppearanceMode == "light" && normalized.ChatFontSize == 22 && normalized.AppearanceAccent is null,
                "loading malformed local reading preferences safely normalizes only the presentation settings");
            File.WriteAllText(layoutPath, "{\"LayoutVersion\":4}");
            var legacy = new LayoutStateService().Load();
            Check(legacy.AppearanceMode == "light" && legacy.ChatFontSize == 15 && legacy.AppearanceAccent is null &&
                legacy.AppearancePaletteId == AppearanceService.DefaultPaletteId,
                "older layout files without new fields keep light Mist blue at 15px with no custom accent");
        }
        finally { File.WriteAllText(layoutPath, stored); }

        void CheckStored(string description)
        {
            var saved = new LayoutStateService().Load();
            Check(saved.AppearanceMode == AppearanceService.Mode && saved.ChatFontSize == AppearanceService.ChatFontSize &&
                saved.AppearanceAccent == AppearanceService.CustomAccent && saved.AppearancePaletteId == AppearanceService.Current.Id &&
                ShellField<LayoutState>("_layout").AppearanceMode == saved.AppearanceMode &&
                ShellField<LayoutState>("_layout").ChatFontSize == saved.ChatFontSize &&
                ShellField<LayoutState>("_layout").AppearanceAccent == saved.AppearanceAccent,
                description);
        }

        async Task ObserveAsync(string label)
        {
            await SettleAsync();
            var observation = await ObserveReadingAppearanceAsync(transcript);
            evidence.Add(new { Label = label, Mode = AppearanceService.Mode, Size = AppearanceService.ChatFontSize,
                Accent = AppearanceService.CustomAccent, Palette = AppearanceService.Current, Observation = observation });
            Check(observation.Text == AppearanceService.Current.Text && observation.AccentText == AppearanceService.Current.AccentText &&
                observation.Error == AppearanceService.Current.Error && observation.CodeComment == AppearanceService.Current.CodeComment &&
                observation.FontSize == AppearanceService.ChatFontSize.ToString("0.#", System.Globalization.CultureInfo.InvariantCulture) + "px" &&
                observation.ColorScheme.Contains(AppearanceService.Current.IsDark ? "dark" : "light", StringComparison.Ordinal),
                label + " synchronizes real transcript semantic colors, color scheme and exact chat font size");
            Check(observation.SameArticle && observation.SameSelection && observation.SelectedText.Length == 20 &&
                Prompt.Text == draft && ActiveId == conversationId &&
                JsonSerializer.Serialize(_shell.ActiveMessages.Select(row => row.Message)) == formalMessages && _gateway.Writes == 0,
                label + " preserves message DOM, exact selected text, unsent draft and formal message objects without gateway writes");
            Check(root.RequestedTheme == (AppearanceService.Current.IsDark ? ElementTheme.Dark : ElementTheme.Light) &&
                ((FrameworkElement)Window.Content).RequestedTheme == root.RequestedTheme &&
                AppearanceService.GetBrush("KynxaReplySurfaceBrush").Color == AppearanceService.ParseColor(AppearanceService.Current.ReplySurface) &&
                AppearanceService.GetBrush("KynxaErrorBrush").Color == AppearanceService.ParseColor(AppearanceService.Current.Error) &&
                AppearanceService.GetBrush("KynxaOnAccentBrush").Color == AppearanceService.ParseColor(AppearanceService.Current.OnAccent),
                label + " updates both open native windows and shared reply, error and on-accent brushes");
            Check(ReferenceEquals(applicationAccentBrush, AppearanceService.GetBrush("KynxaAccentBrush")) &&
                ReferenceEquals(nativeAccentBrush, AppearanceService.GetBrush("AccentFillColorDefaultBrush")) &&
                nativeAccentBrush.Color == AppearanceService.ParseColor(AppearanceService.Current.Accent),
                label + " mutates the original application and native override brush objects without reinserting or replacing their references");
            if (AppearanceService.Current is { IsDark: true, IsHighContrast: false })
            {
                Check(Element<Grid>("MainRegion").Background is SolidColorBrush mainBackground &&
                    mainBackground.Color == AppearanceService.ParseColor("#0C0C0C") &&
                    AppearanceService.GetBrush("KynxaMainBrush").Color == mainBackground.Color,
                    label + " presents the neutral near-black canvas on the actual native work region");
                Check(Element<Grid>("SidebarPane").Background is SolidColorBrush sidebarBackground &&
                    sidebarBackground.Color == AppearanceService.ParseColor("#141414") &&
                    AppearanceService.GetBrush("KynxaSettingsCardBrush").Color == sidebarBackground.Color &&
                    AppearanceService.GetBrush("KynxaReplySurfaceBrush").Color == sidebarBackground.Color,
                    label + " shares the same neutral raised surface across the native sidebar, settings cards and replies");
                string? transcriptBackground = JsonSerializer.Deserialize<string>(await transcript.Browser.ExecuteScriptAsync(
                    "getComputedStyle(document.documentElement).backgroundColor"));
                Check(transcript.Browser.DefaultBackgroundColor == AppearanceService.ParseColor("#0C0C0C") &&
                    transcriptBackground == "rgb(12, 12, 12)",
                    label + " synchronizes the rendered transcript and WebView canvas with the near-black native background");
            }
            CheckStored(label + " reloads all applied preferences from the isolated atomic layout file");
            CheckThemeIcons(label);
        }

        try
        {
            foreach (string mode in new[] { "dark", "system", "light" })
            {
                SelectAppearanceItem(modePicker, mode);
                Check(AppearanceService.Mode == mode, "the real theme picker applies " + mode);
                if (mode == "system")
                {
                    var system = new UISettings();
                    bool systemDark = AppearanceService.ContrastRatio(ToHex(system.GetColorValue(UIColorType.Background)), "#000000") < 11;
                    Check(new AccessibilitySettings().HighContrast || AppearanceService.Current.IsDark == systemDark,
                        "follow-system resolves the current real Windows background theme without modifying OS settings");
                }
                await ObserveAsync("theme " + mode);
                if (!AppearanceService.Current.IsHighContrast && mode is "dark" or "light")
                {
                    // Inspect the rendered SDK Normal state after focus leaves both fields; this is state verification, not physical pointer input.
                    // 焦点离开两个字段后检查实际 SDK Normal 呈现；这是状态验证，不声称执行了真实鼠标输入。
                    chooseColor.Focus(FocusState.Programmatic);
                    modePicker.ApplyTemplate();
                    accentInput.ApplyTemplate();
                    bool choiceNormal = VisualStateManager.GoToState(modePicker, "Normal", false);
                    bool inputNormal = VisualStateManager.GoToState(accentInput, "Normal", false);
                    await SettleAsync();
                    var choiceBackground = NativeUi.ByName<Border>(modePicker, "Background").Background as SolidColorBrush;
                    var inputBackground = NativeUi.ByName<Border>(accentInput, "BorderElement").Background as SolidColorBrush;
                    Color darkControlColor = AppearanceService.ParseColor("#141414");
                    bool darkControls = mode == "dark";
                    Check(choiceNormal && modePicker.FocusState == FocusState.Unfocused && choiceBackground is not null &&
                        (choiceBackground.Color == darkControlColor) == darkControls,
                        "theme " + mode + " updates the actual unfocused SDK ComboBox background without retaining dark-only paint in light mode");
                    Check(inputNormal && accentInput.FocusState == FocusState.Unfocused && inputBackground is not null &&
                        (inputBackground.Color == darkControlColor) == darkControls,
                        "theme " + mode + " updates the actual unfocused SDK TextBox background without retaining dark-only paint in light mode");
                }
                if (mode == "system")
                {
                    int systemRefreshes = 0;
                    void Refreshed(object? sender, EventArgs args) => systemRefreshes++;
                    AppearanceService.Changed += Refreshed;
                    try
                    {
                        string savedBeforeNotification = File.ReadAllText(layoutPath);
                        // Send notifications only to fixture-owned HWNDs; never change the user's Windows theme.
                        // 只给夹具自己的 HWND 发送通知，不修改用户的 Windows 主题。
                        foreach (uint message in new uint[] { 0x031A, 0x001A, 0x0015 })
                        {
                            int before = systemRefreshes;
                            SendAppearanceWindowMessage(WinRT.Interop.WindowNative.GetWindowHandle(Window), message, 0, 0);
                            await WaitAsync(() => systemRefreshes > before,
                                "the owned native HWND notification 0x" + message.ToString("X4") + " re-reads the actual system appearance");
                            Check(AppearanceService.Mode == "system" && File.ReadAllText(layoutPath) == savedBeforeNotification,
                                "native system-appearance refresh does not persist temporary OS colors or replace the selected mode");
                        }
                        await ObserveAsync("native system appearance notifications");
                    }
                    finally { AppearanceService.Changed -= Refreshed; }
                }
            }

            double anchorTop = JsonSerializer.Deserialize<double>(await transcript.Browser.ExecuteScriptAsync("""
                (() => {
                  // A partly clipped first paragraph is the reader's actual anchor; later paragraphs move when its line box changes.
                  // 顶部部分裁切的首段才是真正阅读锚点；字号改变其行高时，后续完整段落的相对位置会正常变化。
                  const node = [...document.querySelectorAll('.message-body > p')].find(item => {
                    const rect=item.getBoundingClientRect(); return rect.height > 0 && rect.bottom > 0 && rect.top < innerHeight;
                  });
                  if (!node) throw new Error('No visible reading anchor');
                  window.__appearanceReadingAnchor={node,top:node.getBoundingClientRect().top};
                  return window.__appearanceReadingAnchor.top;
                })()
                """));
            foreach (double size in new[] { 13d, 13.5, 18, 22, 15 })
            {
                var anchorBefore = await ObserveAppearanceAnchorGeometryAsync(transcript);
                SelectAppearanceItem(fontPicker, size);
                await ObserveAsync("chat size " + size);
                var observation = await ObserveReadingAppearanceAsync(transcript);
                // Retain the measured geometry before asserting, including failures in the real Shell host.
                // 在真实 Shell 宿主断言前保留几何实测值，失败轮次也能核对选择的段落和正式锚点候选。
                evidence.Add(new { Label = "font anchor " + size, ExpectedAnchorTop = anchorTop,
                    Before = anchorBefore, After = await ObserveAppearanceAnchorGeometryAsync(transcript), Observation = observation });
                File.WriteAllText(Path.Combine(_directory, "shell-appearance-reading-observations.json"),
                    JsonSerializer.Serialize(evidence, new JsonSerializerOptions { WriteIndented = true }));
                Check(observation.SameAnchor && Math.Abs(observation.AnchorTop - anchorTop) < 2 &&
                    observation.AnchorText.Contains("APPEARANCE_SCROLL_", StringComparison.Ordinal),
                    "font size " + size + " retains the same visible paragraph and its viewport offset");
            }

            SelectAppearanceItem(modePicker, "dark");
            await SettleAsync();
            stored = File.ReadAllText(layoutPath);
            string? savedAccentBeforePicker = AppearanceService.CustomAccent;
            string inputBeforePicker = accentInput.Text;
            bool resetEnabledBeforePicker = resetAccent.IsEnabled;
            chooseColor.StartBringIntoView();
            await SettleAsync();
            colorFlyout.ShowAt(chooseColor);
            await WaitAsync(() => colorPicker.IsLoaded && NativeUi.IsVisible(colorPicker),
                "the production color chooser opens its real native ColorPicker flyout");
            Check(accentInput.Text == inputBeforePicker && resetAccent.IsEnabled == resetEnabledBeforePicker &&
                AppearanceService.CustomAccent == savedAccentBeforePicker && File.ReadAllText(layoutPath) == stored,
                "opening the color chooser initializes its preview without replacing the HEX input or creating a draft");
            colorPicker.Color = AppearanceService.ParseColor("#C05289");
            await SettleAsync();
            Check(accentInput.Text == "#C05289" && AppearanceService.CustomAccent == savedAccentBeforePicker &&
                File.ReadAllText(layoutPath) == stored,
                "the real ColorPicker ColorChanged event updates exact HEX without saving or applying the pending color");
            AppearanceService.Apply(AppearanceService.Current.Id);
            await SettleAsync();
            Check(accentInput.Text == "#C05289" && AppearanceService.CustomAccent == savedAccentBeforePicker &&
                File.ReadAllText(layoutPath) == stored,
                "a same-preference appearance refresh keeps the unapplied picker HEX while its flyout owns focus");
            string? savedModeChoice = modePicker.SelectedKey;
            DateTime savedBeforeLanguage = File.GetLastWriteTimeUtc(layoutPath);
            foreach (string language in new[] { "en", "zh-CN" })
            {
                UiText.Initialize(language);
                await SettleAsync();
                Check(accentInput.Text == "#C05289" && AppearanceService.CustomAccent == savedAccentBeforePicker &&
                    File.ReadAllText(layoutPath) == stored,
                    "language refresh " + language + " keeps the unapplied HEX without saving or applying it");
                // Read the selected radio segment's actual rendered label, not an unrelated hidden option.
                // 读取选中单选分段实际显示的标签，不以其他隐藏选项的文字代替可见文本。
                modePicker.UpdateLayout();
                var selectedModeButton = NativeUi.ById<RadioButton>(modePicker, "Segment_dark");
                string displayedMode = string.Join("", NativeUi.Descendants<TextBlock>(selectedModeButton)
                    .Where(NativeUi.IsVisible).Select(text => text.Text));
                Check(displayedMode == (language == "en" ? "Dark" : "深色") &&
                    modePicker.SelectedKey == savedModeChoice && selectedModeButton.IsChecked == true &&
                    File.GetLastWriteTimeUtc(layoutPath) == savedBeforeLanguage && accentInput.Text == "#C05289",
                    "language refresh " + language + " updates the real theme selection text without re-saving or disturbing the color draft");
            }
            UiText.Initialize(originalLanguage);
            colorFlyout.Hide();
            await SettleAsync();
            foreach (string mode in new[] { "light", "dark" })
            {
                SelectAppearanceItem(modePicker, mode);
                await SettleAsync();
                Check(AppearanceService.Mode == mode && accentInput.Text == "#C05289" &&
                    AppearanceService.CustomAccent == savedAccentBeforePicker && !status.IsOpen,
                    "saving theme " + mode + " keeps the pending HEX separate from the effective color");
            }
            Check(File.ReadAllText(layoutPath) == stored,
                "returning to the original theme leaves all saved preferences unchanged while keeping the color draft");
            string pendingPaletteId = AppearanceService.Current.Id;
            string otherPaletteId = AppearanceService.Palettes.First(palette => palette.Id != pendingPaletteId).Id;
            NativeUi.Invoke(NativeUi.ById<Button>(root, "AppearancePalette_" + otherPaletteId));
            await SettleAsync();
            Check(AppearanceService.Current.Id == otherPaletteId && accentInput.Text == "#C05289" &&
                AppearanceService.CustomAccent == savedAccentBeforePicker && !status.IsOpen,
                "choosing a preset keeps the saved custom-accent policy and the separate unapplied HEX draft");
            NativeUi.Invoke(NativeUi.ById<Button>(root, "AppearancePalette_" + pendingPaletteId));
            await SettleAsync();
            Check(accentInput.Text == "#C05289" && File.ReadAllText(layoutPath) == stored,
                "returning to the original preset preserves the pending HEX and restores the exact saved preference file");
            var expectedFontChange = JsonSerializer.Deserialize<LayoutState>(stored)!;
            expectedFontChange.ChatFontSize = 18;
            SelectAppearanceItem(fontPicker, 18d);
            await SettleAsync();
            Check(AppearanceService.ChatFontSize == 18 && accentInput.Text == "#C05289" &&
                AppearanceService.CustomAccent == savedAccentBeforePicker && !status.IsOpen &&
                File.ReadAllText(layoutPath) == JsonSerializer.Serialize(expectedFontChange),
                "saving a real font choice changes only ChatFontSize in the file and retains the unapplied HEX draft");
            SelectAppearanceItem(fontPicker, 15d);
            await SettleAsync();
            Check(accentInput.Text == "#C05289" && File.ReadAllText(layoutPath) == stored,
                "restoring the reading size also preserves the pending color and original saved preferences");
            NativeUi.SetText(accentInput, "#c05289");
            NativeUi.Invoke(applyAccent);
            Check(AppearanceService.CustomAccent == "#C05289" && accentInput.Text == "#C05289" && !status.IsOpen,
                "Apply color commits the picker choice and replaces its draft with the normalized saved HEX");
            await ObserveAsync("native color picker applied");
            NativeUi.Invoke(resetAccent);
            Check(AppearanceService.CustomAccent is null && accentInput.Text.Length == 0 && !resetAccent.IsEnabled && !status.IsOpen,
                "Use preset color clears both the saved picker color and its HEX draft after a successful save");
            CheckStored("resetting the picker-selected custom color persists the preset color without changing theme or size");
            foreach (string accent in new[] { "#FFFFFF", "#FFFF00", "#000000", "#38BDF8" })
            {
                NativeUi.SetText(accentInput, accent);
                NativeUi.Invoke(applyAccent);
                Check(AppearanceService.CustomAccent == accent && !status.IsOpen,
                    "the actual Apply color button accepts and retains " + accent);
                await ObserveAsync("custom accent " + accent);
                var palette = AppearanceService.Current;
                Check(palette.IsHighContrast ||
                    AppearanceService.ContrastRatio(palette.Accent, palette.OnAccent) >= 4.5 &&
                    AppearanceService.ContrastRatio(palette.AccentText, palette.Main) >= 4.5 &&
                    AppearanceService.ContrastRatio(palette.AccentText, palette.Soft) >= 4.5,
                    "custom accent " + accent + " keeps button text and linked text at least 4.5:1 while preserving the requested HEX separately");
            }
            foreach (var preset in AppearanceService.Palettes)
            foreach (bool dark in new[] { false, true })
            foreach (string accent in new[] { "#FFFFFF", "#FFFF00", "#000000", "#38BDF8" })
            {
                var palette = AppearanceService.ResolvePalette(preset, dark, accent);
                if (dark)
                {
                    // Accent-tinted selection stays separate from the neutral structural surfaces.
                    // 强调色选中态与中性的结构表面分离，不能把工作区重新染成蓝灰色。
                    Check(palette.Main == "#0C0C0C" && palette.Sidebar == "#141414" && palette.ReplySurface == palette.Sidebar &&
                        palette.Border == "#353535" && palette.Text == "#ECECEC" && palette.ControlHover == "#202020" &&
                        palette.ControlPressed == "#292929" && palette.Disabled == "#303030",
                        preset.Id + " dark " + accent + " keeps the requested neutral near-black surface and interaction palette");
                    Check(new[] { palette.Main, palette.Sidebar, palette.ReplySurface, palette.Border, palette.Text,
                        palette.Secondary, palette.Muted, palette.ControlHover, palette.ControlPressed, palette.Disabled }
                        .Select(AppearanceService.ParseColor).All(color => color.R == color.G && color.G == color.B) &&
                        AppearanceService.ParseColor(palette.Main).R < AppearanceService.ParseColor(palette.Sidebar).R &&
                        AppearanceService.ParseColor(palette.Sidebar).R < AppearanceService.ParseColor(palette.ControlHover).R &&
                        AppearanceService.ParseColor(palette.ControlHover).R < AppearanceService.ParseColor(palette.ControlPressed).R,
                        preset.Id + " dark " + accent + " retains neutral channels and visible main, raised, hover and pressed depth");
                }
                Check(AppearanceService.ContrastRatio(palette.Accent, palette.OnAccent) >= 4.5 &&
                    AppearanceService.ContrastRatio(palette.Text, palette.Main) >= 4.5 &&
                    AppearanceService.ContrastRatio(palette.AccentText, palette.Soft) >= 4.5 &&
                    AppearanceService.ContrastRatio(palette.CodeComment, palette.Sidebar) >= 4.5,
                    preset.Id + (dark ? " dark " : " light ") + accent + " resolves accessible text, code comments and extreme custom accent pairs");
                foreach (string semanticText in new[] { palette.Text, palette.Secondary, palette.Muted,
                    palette.Success, palette.Error, palette.Warning, palette.AccentText })
                    Check(new[] { palette.Main, palette.Sidebar, palette.Soft }.All(background =>
                        AppearanceService.ContrastRatio(semanticText, background) >= 4.5),
                        preset.Id + (dark ? " dark " : " light ") + accent + " keeps semantic text " + semanticText +
                        " at least 4.5:1 against its weakest Main, Sidebar or Soft background");
            }

            stored = File.ReadAllText(layoutPath);
            string selectedAccent = AppearanceService.CustomAccent!;
            NativeUi.SetText(accentInput, "#GGGGGG");
            NativeUi.Invoke(applyAccent);
            Check(status.IsOpen && AppearanceService.CustomAccent == selectedAccent && File.ReadAllText(layoutPath) == stored,
                "an invalid typed HEX reports an inline settings error without replacing saved or displayed colors");

            string blockedPath = layoutPath + ".tmp";
            Directory.CreateDirectory(blockedPath);
            try
            {
                SelectAppearanceItem(modePicker, "light");
                Check(AppearanceService.Mode == "dark" && modePicker.SelectedKey == "dark" &&
                    accentInput.Text == "#GGGGGG" && status.IsOpen,
                    "a real mode-save failure rolls the actual theme picker back and preserves the unapplied HEX");
                SelectAppearanceItem(fontPicker, 22d);
                Check(AppearanceService.ChatFontSize == 15 && (double)((ComboBoxItem)fontPicker.SelectedItem).Tag == 15 &&
                    accentInput.Text == "#GGGGGG" && status.IsOpen,
                    "a real font-save failure restores the reading size and preserves the unapplied HEX");
                NativeUi.SetText(accentInput, "#FF0000");
                NativeUi.Invoke(applyAccent);
                Check(AppearanceService.CustomAccent == selectedAccent && File.ReadAllText(layoutPath) == stored &&
                    accentInput.Text == "#FF0000" && status.IsOpen,
                    "a real custom-color save failure preserves the pending HEX, saved file and effective color");
                NativeUi.Invoke(resetAccent);
                Check(AppearanceService.CustomAccent == selectedAccent && File.ReadAllText(layoutPath) == stored &&
                    accentInput.Text == "#FF0000" && status.IsOpen,
                    "Use preset color preserves the pending HEX and saved custom color when its save fails");
                NativeUi.Invoke(reset);
                Check(AppearanceService.Mode == "dark" && AppearanceService.CustomAccent == selectedAccent &&
                    File.ReadAllText(layoutPath) == stored && accentInput.Text == "#FF0000" && status.IsOpen,
                    "restore defaults also preserves the pending HEX and rolls back atomically when its save fails");
            }
            finally { Directory.Delete(blockedPath); }

            NativeUi.Invoke(resetAccent);
            Check(AppearanceService.CustomAccent is null && accentInput.Text.Length == 0 && !resetAccent.IsEnabled && !status.IsOpen &&
                AppearanceService.Mode == "dark" && AppearanceService.ChatFontSize == 15,
                "Use preset color removes only the custom accent and keeps the selected theme and reading size");
            SelectAppearanceItem(fontPicker, 18d);
            NativeUi.SetText(accentInput, "#ABCDEF");
            NativeUi.Invoke(applyAccent);
            await ObserveAsync("saved dark 18px custom settings");
            CheckStored("saved non-default theme, reading size and custom accent survive a fresh local settings load");
            applyAccent.StartBringIntoView();
            await SettleAsync();
            await NativeWindowCapture.CaptureAsync(settings, Path.Combine(_directory, "shell-appearance-dark-custom-settings.png"));
            await CapturePresentedAsync("shell-appearance-dark-transcript.png");
            NativeUi.Invoke(reset);
            Check(AppearanceService.Current.Id == AppearanceService.DefaultPaletteId && AppearanceService.Mode == "light" &&
                AppearanceService.ChatFontSize == 15 && AppearanceService.CustomAccent is null && accentInput.Text.Length == 0 &&
                !reset.IsEnabled && !status.IsOpen,
                "the real Restore default appearance button resets theme, font size, preset, custom accent and HEX draft together");
            CheckStored("restoring default appearance persists the entire default preference set");
            NativeUi.SetText(accentInput, "#C05289");
            await SettleAsync();
            Check(resetAccent.IsEnabled && reset.IsEnabled,
                "pending HEX enables both discard actions even when the saved appearance is already the default");
            NativeUi.Invoke(resetAccent);
            Check(accentInput.Text.Length == 0 && AppearanceService.CustomAccent is null && !resetAccent.IsEnabled && !reset.IsEnabled,
                "Use preset color can discard an unapplied HEX without first applying it");
            NativeUi.SetText(accentInput, "#C05289");
            await SettleAsync();
            NativeUi.Invoke(reset);
            Check(accentInput.Text.Length == 0 && AppearanceService.CustomAccent is null && !resetAccent.IsEnabled && !reset.IsEnabled,
                "Restore defaults can discard an unapplied HEX while keeping the saved default appearance");
            CheckStored("discarding a pending color keeps the complete saved default preference set");

            var highContrastColors = new Dictionary<UIElementType, Color>
            {
                [UIElementType.Window] = AppearanceService.ParseColor("#000000"),
                [UIElementType.WindowText] = AppearanceService.ParseColor("#FFFFFF"),
                [UIElementType.Highlight] = AppearanceService.ParseColor("#FFFF00"),
                [UIElementType.HighlightText] = AppearanceService.ParseColor("#000000"),
                [UIElementType.Hotlight] = AppearanceService.ParseColor("#00FFFF"),
                [UIElementType.ButtonFace] = AppearanceService.ParseColor("#000000"),
                [UIElementType.GrayText] = AppearanceService.ParseColor("#808080")
            };
            var contrastPalette = AppearanceService.ResolveHighContrastPalette(AppearanceService.Palettes[1], type => highContrastColors[type]);
            Check(contrastPalette.IsHighContrast && contrastPalette.IsDark && contrastPalette.Main == "#000000" &&
                contrastPalette.Text == "#FFFFFF" && contrastPalette.Accent == "#FFFF00" &&
                contrastPalette.Selection == "#FFFF00" && contrastPalette.OnAccent == "#000000" &&
                contrastPalette.AccentText == "#00FFFF" && contrastPalette.CodeComment == "#FFFFFF" &&
                AppearanceService.Mode == "light" && AppearanceService.CustomAccent is null,
                "pure high-contrast resolution honors supplied system Window/Highlight/HighlightText colors without changing OS or saved preferences");
            File.WriteAllText(Path.Combine(_directory, "shell-appearance-reading-observations.json"),
                JsonSerializer.Serialize(evidence, new JsonSerializerOptions { WriteIndented = true }));
        }
        finally
        {
            colorFlyout.Hide();
            UiText.Initialize(originalLanguage);
            AppearanceService.ApplyPreferences(originalId, originalMode, originalFontSize, originalAccent);
            var layout = ShellField<LayoutState>("_layout");
            layout.AppearancePaletteId = originalId; layout.AppearanceMode = originalMode;
            layout.ChatFontSize = originalFontSize; layout.AppearanceAccent = originalAccent;
            new LayoutStateService().Save(layout);
            await transcript.Browser.ExecuteScriptAsync("delete window.__appearanceReadingAnchor");
            await SettleAsync();
        }
    }

    private static string ToHex(Color color) => $"#{color.R:X2}{color.G:X2}{color.B:X2}";

    [DllImport("user32.dll", EntryPoint = "SendMessageW", ExactSpelling = true)]
    private static extern nint SendAppearanceWindowMessage(nint window, uint message, nuint wParam, nint lParam);
}
