using System.Text.Json;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Automation.Provider;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;

namespace KYNXA_Desktop;

public partial class App
{
    // Inspect loaded production text and popup layout; a configured font name alone is insufficient.
    // 检查实际加载的生产文字与弹出层布局；仅有配置中的字体名称不足以证明显示正确。
    private async Task CheckTypographyAsync()
    {
        string originalLanguage = UiText.Language;
        string originalDraft = Prompt.Text;
        Guid? originalChatId = ActiveId;
        string originalMessages = JsonSerializer.Serialize(_shell.ActiveMessages.Select(row => row.Message));
        string originalCatalog = JsonSerializer.Serialize(_gateway.Catalog);
        string expectedFont = ((FontFamily)Resources["KynxaUIFont"]).Source;
        var observations = new List<object>();
        Window? settings = null;
        ContentDialog? dialog = null;
        try
        {
            Check(expectedFont.StartsWith("Segoe UI Variable Text, Segoe UI", StringComparison.Ordinal) &&
                expectedFont.Contains("Microsoft YaHei UI", StringComparison.Ordinal),
                "the shared UI stack prioritizes the Latin UI face and includes the Chinese UI fallback");
            Check(Prompt.IsLoaded && Prompt.ActualWidth > 0 && Prompt.FontFamily.Source == expectedFont,
                "the loaded production composer uses the common bilingual font stack");
            var sidebarTexts = NativeUi.Descendants<TextBlock>(Element<Grid>("ShellGrid"))
                .Where(text => NativeUi.IsVisible(text) && text.ActualWidth > 0 &&
                    text.Text.Any(character => character is >= '\u4e00' and <= '\u9fff')).Take(8).ToArray();
            Check(sidebarTexts.Length >= 3 && sidebarTexts.All(text => text.FontFamily.Source == expectedFont),
                "loaded Chinese sidebar labels and controls use the shared UI font");
            foreach (string language in new[] { "zh-CN", "en" })
            {
                UiText.Initialize(language);
                await SettleAsync();
                var more = NativeUi.Descendants<Button>((FrameworkElement)Window.Content)
                    .Single(button => AutomationProperties.GetName(button) == UiText.Get("更多功能"));
                NativeUi.Invoke(more);
                await WaitAsync(() => TypographyMenuItems().Any(item => item.Text == UiText.Get("新建聊天 Ctrl+N")),
                    "the real title menu loads its " + language + " labels");
                await SettleAsync();
                var items = TypographyMenuItems();
                Check(items.Length == 8 && items.All(item => item.FontFamily.Source == expectedFont),
                    "all eight real navigation menu items share the UI font in " + language);
                var labels = items.Select(item => new
                {
                    Item = item,
                    Text = NativeUi.Descendants<TextBlock>(item).Single(text => text.Text == item.Text)
                }).ToArray();
                Check(labels.All(label => label.Text.IsLoaded && label.Text.ActualWidth > 0 &&
                    label.Text.ActualHeight > 0 && label.Text.FontFamily.Source == expectedFont && !label.Text.IsTextTrimmed),
                    "native menu template text is loaded, untrimmed and uses the requested UI font in " + language);
                Check(labels.All(label =>
                {
                    Rect bounds = label.Text.TransformToVisual(label.Item).TransformBounds(
                        new Rect(0, 0, label.Text.ActualWidth, label.Text.ActualHeight));
                    return bounds.Left >= -1 && bounds.Top >= -1 && bounds.Right <= label.Item.ActualWidth + 1 &&
                        bounds.Bottom <= label.Item.ActualHeight + 1 && IsInsidePopup(label.Text);
                }), "every visible navigation label stays inside its real menu item and popup in " + language);
                Check(new[] { "新建聊天 Ctrl+N", "搜索会话 Ctrl+F", "聚焦输入 Ctrl+L" }
                    .All(key => labels.Any(label => label.Text.Text == UiText.Get(key))),
                    "Chinese/English navigation labels retain complete Ctrl+N, Ctrl+F and Ctrl+L shortcuts in " + language);
                observations.Add(new
                {
                    Surface = "navigation", Language = language,
                    Labels = labels.Select(label => new { label.Text.Text, Font = label.Text.FontFamily.Source,
                        label.Text.ActualWidth, label.Text.ActualHeight, label.Text.IsTextTrimmed }).ToArray()
                });
                // PrintWindow captures the owning window; menu typography is verified through its realized controls above.
                // PrintWindow 捕获所属窗口；菜单本身的字体与边界由上方已实现的真实控件检查。
                await CapturePresentedAsync("shell-typography-window-" + language + ".png");
                var focusItem = items.Single(item => item.Text == UiText.Get("聚焦输入 Ctrl+L"));
                if (FrameworkElementAutomationPeer.CreatePeerForElement(focusItem)?.GetPattern(PatternInterface.Invoke)
                    is not IInvokeProvider invoke) throw new InvalidOperationException("The real focus menu item has no Invoke pattern.");
                invoke.Invoke();
                await WaitAsync(() => TypographyMenuItems().Length == 0, "the native menu closes after focusing input");
            }

            UiText.Initialize("zh-CN");
            Call("StorageSettings_Click", _shell, new RoutedEventArgs());
            settings = ShellField<Window>("_storageSettingsWindow");
            var settingsRoot = (FrameworkElement)settings.Content;
            await WaitAsync(() => settingsRoot.IsLoaded && settingsRoot.XamlRoot is not null,
                "the production settings window loads before font inspection");
            await SettleAsync();
            var languagePicker = NativeUi.ById<ComboBox>(settingsRoot, "InterfaceLanguagePicker");
            var settingsLabels = NativeUi.Descendants<TextBlock>(settingsRoot)
                .Where(text => !string.IsNullOrWhiteSpace(text.Text) && text.Text.Any(char.IsLetter)).ToArray();
            File.WriteAllText(Path.Combine(_directory, "shell-typography-settings-fonts.json"), JsonSerializer.Serialize(
                settingsLabels.Select(text => new { text.Text, Font = text.FontFamily.Source }),
                new JsonSerializerOptions { WriteIndented = true }));
            Check(languagePicker.FontFamily.Source == expectedFont && settingsLabels.Length >= 10 &&
                settingsLabels.All(text => text.FontFamily.Source == expectedFont),
                "real settings labels and the language picker share the bilingual UI font");
            foreach (string language in new[] { "en", "zh-CN" })
            {
                UiText.Initialize(language);
                await WaitAsync(() => NativeUi.Descendants<TextBlock>(settingsRoot).Any(text => text.Text == UiText.Get("界面语言")),
                    "the existing settings window translates its label to " + language);
                Check(NativeUi.Descendants<TextBlock>(settingsRoot).Where(text => !string.IsNullOrWhiteSpace(text.Text) &&
                    text.Text.Any(char.IsLetter)).All(text => text.FontFamily.Source == expectedFont),
                    "live settings translation preserves the shared font in " + language);
            }
            observations.Add(new { Surface = "settings", Font = languagePicker.FontFamily.Source,
                Labels = settingsLabels.Select(text => new { text.Text, Font = text.FontFamily.Source }).ToArray() });
            await NativeWindowCapture.CaptureAsync(settings, Path.Combine(_directory, "shell-typography-settings.png"));
            settings.Close();
            settings = null;
            Window.Activate();

            // Load compatibility Markdown and explicit symbol/monospace content under a real modal root.
            // 将兼容 Markdown、显式符号和等宽内容加载到真实模态根，验证全局字体不会覆盖语义专用字体。
            var markdown = new MarkdownReply { Text = "中文正文 English body\n\n```csharp\nvar message = \"中文 Code\";\n```" };
            var icon = new FontIcon { Glyph = "\uE700", FontSize = 20 };
            var content = new StackPanel { Spacing = 12, MinWidth = 360 };
            content.Children.Add(markdown);
            content.Children.Add(icon);
            foreach (string family in new[] { expectedFont, "Microsoft YaHei UI", "SimSun" })
            {
                content.Children.Add(new TextBlock { Text = family, FontSize = 10 });
                content.Children.Add(new TextBlock { Text = "中文界面 字体检查 简体中文", FontSize = 14,
                    FontFamily = new FontFamily(family) });
            }
            dialog = new ContentDialog { XamlRoot = _shell.XamlRoot, Title = "中文标题 English title",
                Content = content, CloseButtonText = "关闭 Close" };
            var showing = dialog.ShowAsync();
            await WaitAsync(() => markdown.Document.IsLoaded && markdown.Document.Blocks.OfType<Paragraph>()
                .Any(paragraph => paragraph.FontFamily.Source.Contains("Cascadia", StringComparison.Ordinal)),
                "the actual compatibility Markdown loads body and code inside a native ContentDialog");
            var code = markdown.Document.Blocks.OfType<Paragraph>().Single(paragraph =>
                paragraph.FontFamily.Source.Contains("Cascadia", StringComparison.Ordinal));
            Check(dialog.FontFamily.Source == expectedFont && markdown.Document.FontFamily.Source == expectedFont,
                "the real modal and loaded compatibility Markdown body inherit the common UI font");
            Check(code.FontFamily.Source.Contains("Consolas", StringComparison.Ordinal) &&
                string.Concat(code.Inlines.OfType<Run>().Select(run => run.Text)).Contains("var message", StringComparison.Ordinal),
                "actual Markdown code retains its monospace stack and source text");
            Check(icon.IsLoaded && icon.ActualWidth > 0 && icon.ActualHeight > 0 &&
                icon.FontFamily.Source != expectedFont && icon.FontFamily.Source.Contains("Segoe", StringComparison.Ordinal),
                "loaded FontIcon retains its symbol face and nonempty geometry");
            observations.Add(new { Surface = "modal-markdown", BodyFont = markdown.Document.FontFamily.Source,
                CodeFont = code.FontFamily.Source, IconFont = icon.FontFamily.Source });
            await CapturePresentedAsync("shell-typography-modal-markdown.png");
            dialog.Hide();
            await showing;
            dialog = null;
            Check(Prompt.Text == originalDraft && ActiveId == originalChatId &&
                JsonSerializer.Serialize(_shell.ActiveMessages.Select(row => row.Message)) == originalMessages &&
                JsonSerializer.Serialize(_gateway.Catalog) == originalCatalog && _gateway.Writes == 0,
                "font/menu/settings inspection preserves draft, active chat, formal messages and catalog without gateway writes");
            File.WriteAllText(Path.Combine(_directory, "shell-typography.json"),
                JsonSerializer.Serialize(observations, new JsonSerializerOptions { WriteIndented = true }));
        }
        finally
        {
            dialog?.Hide();
            settings?.Close();
            UiText.Initialize(originalLanguage);
            NativeUi.SetText(Prompt, originalDraft);
            Window.Activate();
        }
    }

    private MenuFlyoutItem[] TypographyMenuItems() => VisualTreeHelper.GetOpenPopupsForXamlRoot(_shell.XamlRoot)
        .Where(popup => popup.IsOpen && popup.Child is not null)
        .SelectMany(popup => NativeUi.Descendants<MenuFlyoutItem>(popup.Child!)).ToArray();

}
