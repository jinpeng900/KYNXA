using System.Diagnostics;
using System.Runtime.InteropServices.WindowsRuntime;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;
using Windows.UI;
using Windows.UI.ViewManagement;

namespace TranscriptUiSmoke;

public partial class App
{
    /// <summary>
    /// The unknown-outcome card shares the queue slot above a composer stand-in; no dialog or window is created.
    /// 结果未知提示卡与排队区共用输入框上方位置；检查不创建对话框或新窗口，也不读取用户数据。
    /// </summary>
    private void LaunchUnknownOutcomeChecks(Stopwatch cold)
    {
        foreach (string name in new[] { "Colors", "Dimensions", "Controls" })
            Resources.MergedDictionaries.Add(new ResourceDictionary { Source = new Uri($"ms-appx:///Styles/{name}.xaml") });
        AppearanceService.Apply(AppearanceService.DefaultPaletteId);
        var card = new ConversationUnknownOutcomeCard { Margin = new Thickness(16, 0, 16, 0), HorizontalAlignment = HorizontalAlignment.Stretch };
        var queue = new ConversationPromptQueue { Margin = new Thickness(16, 0, 16, 0), HorizontalAlignment = HorizontalAlignment.Stretch };
        var composer = new TextBox { Text = "正在输入的草稿", AcceptsReturn = true, MinHeight = 72, CornerRadius = new CornerRadius(12) };
        var stack = new StackPanel { VerticalAlignment = VerticalAlignment.Bottom, Spacing = 12, Margin = new Thickness(0, 0, 0, 16) };
        stack.Children.Add(card);
        stack.Children.Add(queue);
        stack.Children.Add(new Border { Margin = new Thickness(0), Child = composer });
        var root = new Grid { Background = Resources["KynxaMainBrush"] as Brush, Padding = new Thickness(24, 0, 24, 0) };
        root.Children.Add(stack);
        _window = new Window { Title = "KYNXA unknown outcome card", Content = root };
        AppearanceService.TrackWindow(_window);
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(760, 360));
        root.Loaded += (_, _) => _ = RunUnknownOutcomeChecksAsync(root, card, queue, composer, cold);
        _window.AppWindow.Show(activateWindow: false);
    }

    private async Task RunUnknownOutcomeChecksAsync(Grid root, ConversationUnknownOutcomeCard card,
        ConversationPromptQueue queue, TextBox composer, Stopwatch cold)
    {
        File.WriteAllText(_result, "RUNNING: native unknown-outcome card checks");
        string language = UiText.Language, palette = AppearanceService.Current.Id, mode = AppearanceService.Mode;
        double fontSize = AppearanceService.ChatFontSize;
        string? accent = AppearanceService.CustomAccent;
        var shots = new List<string>();
        try
        {
            UiText.Initialize("zh-CN");
            var pending = new List<QueuedPrompt>
            {
                new(Guid.NewGuid(), "先比较两种方案的成本", "api", "model-a", "full"),
                new(Guid.NewGuid(), "再整理成表格", "api", "model-a", "full")
            };
            queue.ShowMessages(pending, true);
            await Task.Delay(60);
            var queueRows = QueueRows(queue);
            Check(queue.Visibility == Visibility.Visible && queueRows.Length == 2, "queued messages render before the notice appears");
            composer.Focus(FocusState.Programmatic);
            composer.SelectionStart = 3;
            await Task.Delay(30);

            var task = UnknownOutcomeTask();
            var single = new[] { UnknownOperation("round-1-host-run", "terminal.host.run") };
            UnknownOutcomeRequest? confirmed = null, cancelled = null;
            card.ConfirmRequested += (_, request) => confirmed = request;
            card.CancelRequested += (_, request) => cancelled = request;
            card.Show(task, single);
            // Mirror the shell: the card overlays the queue display while the queue data stays intact.
            // 与主界面一致：卡片显示时只隐藏排队区显示，排队数据保持不变。
            card.SetQueuedCount(pending.Count);
            queue.Visibility = Visibility.Collapsed;
            await Task.Delay(60);
            root.UpdateLayout();
            var (surface, summary, buttons) = CardParts(card);
            double cardBottom = card.TransformToVisual(root).TransformPoint(new Windows.Foundation.Point(0, card.ActualHeight)).Y;
            double composerTop = composer.TransformToVisual(root).TransformPoint(new Windows.Foundation.Point(0, 0)).Y;
            _metrics["unknownOutcomeLayout"] = new { cardHeight = card.ActualHeight, cardBottom, composerTop, cardWidth = card.ActualWidth, composerWidth = composer.ActualWidth };
            Check(card.IsShown && card.ActualHeight > 0 && queue.Visibility == Visibility.Collapsed
                && Math.Abs(composerTop - cardBottom - 12) < 1.5 && card.ActualWidth < composer.ActualWidth,
                "the card occupies the queue slot directly above the composer and is slightly narrower");
            Check(summary.Text == "上次任务有 1 个操作结果未知：terminal.host.run", "the single-line summary names the unknown operation");
            Check(CardQueued(card).Visibility == Visibility.Visible && CardQueued(card).Text == "2 条排队消息已保留，处理后恢复显示。",
                "the card states how many hidden queued messages are kept");
            Check(ReferenceEquals(FocusManager.GetFocusedElement(root.XamlRoot), composer) && composer.Text == "正在输入的草稿"
                && composer.SelectionStart == 3, "showing the card keeps focus, draft and caret in the composer");
            Check(ReferenceEquals(surface.Background, Resources["KynxaReplySurfaceBrush"]) && ReferenceEquals(surface.BorderBrush, Resources["KynxaDividerBrush"])
                && surface.CornerRadius.TopLeft == 10 && summary.FontSize == 13
                && ReferenceEquals(QueueRows(queue)[0].Background, surface.Background) && QueueRows(queue)[0].CornerRadius == surface.CornerRadius,
                "the card reuses the queue row surface, border, radius and font size");
            var neutral = Resources["KynxaSecondaryTextBrush"] as Brush;
            Check(buttons.All(button => ReferenceEquals(button.Style, Resources["KynxaQuietButtonStyle"])
                && ReferenceEquals(button.FocusVisualPrimaryBrush, neutral) && button.IsTabStop && button.IsEnabled),
                "both actions use the quiet button style, neutral focus and keyboard tab stops");
            Check(AutomationProperties.GetName(card) == "结果未知的操作" && AutomationProperties.GetName(buttons[0]) == "确认结果未知并继续"
                && AutomationProperties.GetName(buttons[1]) == "取消任务" && AutomationProperties.GetHelpText(buttons[0]).Length > 0,
                "card and actions expose readable automation names");
            shots.Add(await CaptureNativeAsync(root, "light-zh"));

            InvokeQueueControl(buttons[0]);
            await Task.Delay(30);
            Check(confirmed?.OperationIds.SequenceEqual(new[] { "round-1-host-run" }) == true && cancelled is null,
                "confirm raises only the explicit request with the formal operation IDs");
            InvokeQueueControl(buttons[1]);
            await Task.Delay(30);
            Check(cancelled?.Task.TaskId == task.TaskId, "cancel raises its own explicit request");
            card.SetBusy(true);
            Check(buttons.All(button => !button.IsEnabled), "busy state disables both actions while a control is pending");
            card.SetBusy(false);
            card.ShowFeedback("任务控制未确认，排队消息已保留，请稍后重试。");
            await Task.Delay(30);
            Check(CardFeedback(card).Visibility == Visibility.Visible, "an unconfirmed control is explained inside the card, not in a dialog");
            card.ShowFeedback(null);

            var many = new[]
            {
                UnknownOperation("op-1", "terminal.host.run"), UnknownOperation("op-2", "terminal.host.start"),
                UnknownOperation("op-3", "browser.open"), UnknownOperation("op-4", "terminal.host.run")
            };
            card.Show(task, many);
            Check(summary.Text == "上次任务有 4 个操作结果未知：terminal.host.run · terminal.host.start · browser.open …"
                && summary.MaxLines == 2, "several operations are counted and the list stays compact");
            var childA = Guid.NewGuid();
            var childB = Guid.NewGuid();
            var repeatedLocalIds = new[]
            {
                UnknownOperation("same-local-operation", "terminal.host.run") with { TaskId = childA },
                UnknownOperation("same-local-operation", "terminal.host.run") with { TaskId = childB }
            };
            var modelCost = new TaskModelReservation(childA, "unknown-model-reservation", "unknown");
            card.Show(task, repeatedLocalIds, [modelCost]);
            confirmed = null;
            InvokeQueueControl(buttons[0]);
            await Task.Delay(30);
            Check(confirmed?.Operations is { Length: 2 } scoped && scoped[0].TaskId == childA && scoped[1].TaskId == childB
                && scoped[0].OperationId == scoped[1].OperationId
                && confirmed.ModelReservationIds is [{ TaskId: var owner, ReservationId: "unknown-model-reservation" }] && owner == childA,
                "the same inline confirmation retains distinct descendant owners and the exact unknown model reservation");
            card.Show(task, [], [modelCost]);
            confirmed = null;
            Check(summary.Text.Contains("模型请求成本未知") && !summary.Text.Contains("terminal.host.run"),
                "unknown model cost alone renders a usable notice without inventing a failed tool operation");
            InvokeQueueControl(buttons[0]);
            await Task.Delay(30);
            Check(confirmed?.OperationIds.Length == 0 && confirmed.Operations?.Length == 0 && confirmed.ModelReservationIds?.Length == 1,
                "a cost-only notice submits reservation identity without manufacturing operation IDs");
            UiText.Initialize("en");
            await Task.Delay(60);
            Check(summary.Text.Contains(UiText.Get("模型请求成本未知")) && !summary.Text.Contains("模型请求成本未知"),
                "an open unknown-cost notice translates its category without requiring another Show call");
            UiText.Initialize("zh-CN");
            await Task.Delay(60);
            card.Show(task, single);

            UiText.Initialize("en");
            card.Show(task, single);
            await Task.Delay(60);
            Check(summary.Text == "The previous task has 1 operation(s) with an unknown outcome: terminal.host.run"
                && (string)buttons[0].Content == "Accept unknown outcome and resume" && (string)buttons[1].Content == "Cancel task"
                && AutomationProperties.GetName(buttons[1]) == "Cancel task", "English text and automation names follow the language");
            shots.Add(await CaptureNativeAsync(root, "light-en"));

            AppearanceService.ApplyPreferences(AppearanceService.DefaultPaletteId, "dark", 15, null);
            await Task.Delay(120);
            Check(((SolidColorBrush)surface.Background).Color == AppearanceService.ParseColor(AppearanceService.Current.ReplySurface)
                && ((SolidColorBrush)summary.Foreground).Color == AppearanceService.ParseColor(AppearanceService.Current.Text),
                "dark theme updates the shared surface and text brushes in place");
            shots.Add(await CaptureNativeAsync(root, "dark-en"));
            UiText.Initialize("zh-CN");
            await Task.Delay(60);
            shots.Add(await CaptureNativeAsync(root, "dark-zh"));

            var highContrast = AppearanceService.ResolveHighContrastPalette(AppearanceService.Palettes[1], type => type switch
            {
                UIElementType.Window => Color.FromArgb(255, 0, 0, 0),
                UIElementType.Highlight => Color.FromArgb(255, 255, 255, 0),
                UIElementType.HighlightText => Color.FromArgb(255, 0, 0, 0),
                UIElementType.Hotlight => Color.FromArgb(255, 0, 255, 255),
                _ => Color.FromArgb(255, 255, 255, 255)
            });
            // Synthetic OS colors applied to the same shared brush keys; the global accessibility setting is untouched.
            // 合成系统色写入相同的共享画刷键，不切换用户的全局辅助功能设置。
            foreach (var (key, value) in new[]
            {
                ("KynxaMainBrush", highContrast.Main), ("KynxaReplySurfaceBrush", highContrast.ReplySurface),
                ("KynxaDividerBrush", highContrast.Border), ("KynxaTextBrush", highContrast.Text),
                ("KynxaSecondaryTextBrush", highContrast.Secondary), ("KynxaControlHoverBrush", highContrast.ControlHover),
                ("KynxaControlPressedBrush", highContrast.ControlPressed)
            })
                AppearanceService.GetBrush(key).Color = AppearanceService.ParseColor(value);
            await Task.Delay(120);
            double contrast = AppearanceService.ContrastRatio(highContrast.Text, highContrast.ReplySurface);
            Check(contrast >= 4.5, $"high-contrast summary text keeps readable contrast ({contrast:F1}:1)");
            shots.Add(await CaptureNativeAsync(root, "high-contrast-zh"));
            AppearanceService.ApplyPreferences(palette, mode, fontSize, accent);

            card.Hide();
            queue.ShowMessages(pending, true);
            await Task.Delay(60);
            Check(!card.IsShown && CardQueued(card).Visibility == Visibility.Collapsed && queue.Visibility == Visibility.Visible && QueueRows(queue).Length == 2
                && ReferenceEquals(QueueRows(queue)[0], queueRows[0]) && pending.Count == 2,
                "removing the card restores the same queued rows in their original order");
            _metrics["checks"] = _checks;
            _metrics["mode"] = "unknown-outcome-only";
            _metrics["screenshots"] = shots;
            _metrics["durationMs"] = cold.ElapsedMilliseconds;
            File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
            File.WriteAllText(_result, $"PASS: {_checks} native unknown-outcome card checks; queue-slot overlay, retained queue rows, focus and draft preservation, explicit actions, automation names, zh/en, light/dark/synthetic high contrast. Screenshots: {string.Join(", ", shots)}");
        }
        catch (Exception error)
        {
            File.WriteAllText(_result, "FAIL: " + error);
        }
        finally
        {
            UiText.Initialize(language);
            AppearanceService.ApplyPreferences(palette, mode, fontSize, accent);
            _window?.Close();
        }
    }

    private static TaskRecord UnknownOutcomeTask() => new(Guid.NewGuid(), Guid.NewGuid().ToString("D"), "启动示例服务", [],
        new(Guid.NewGuid(), null), null, [], new(256, null), new(2, 0, 0, 0, 10, 20, 0), "waiting-verification",
        4, 2, null, 9, "recovery-evidence-required", 100, 200);

    private static TaskOperation UnknownOperation(string id, string tool) =>
        new(Guid.NewGuid(), Guid.NewGuid(), new(id, "call-" + id, tool, "irreversible"), "unknown", null, null, 300);

    private static (Border Surface, TextBlock Summary, Button[] Buttons) CardParts(ConversationUnknownOutcomeCard card)
    {
        var surface = (Border)((Border)card.Content).Child;
        var layout = (Grid)surface.Child;
        var text = layout.Children.OfType<StackPanel>().First(panel => panel.Children.OfType<TextBlock>().Any());
        var buttons = layout.Children.OfType<StackPanel>().Single(panel => panel.Children.OfType<Button>().Any()).Children.OfType<Button>().ToArray();
        return (surface, text.Children.OfType<TextBlock>().First(), buttons);
    }

    private static TextBlock CardQueued(ConversationUnknownOutcomeCard card)
    {
        var layout = (Grid)((Border)((Border)card.Content).Child).Child;
        return layout.Children.OfType<StackPanel>().First(panel => panel.Children.OfType<TextBlock>().Any()).Children.OfType<TextBlock>().ElementAt(1);
    }

    private static TextBlock CardFeedback(ConversationUnknownOutcomeCard card)
    {
        var layout = (Grid)((Border)((Border)card.Content).Child).Child;
        return layout.Children.OfType<StackPanel>().First(panel => panel.Children.OfType<TextBlock>().Any()).Children.OfType<TextBlock>().Last();
    }

    private static async Task<string> CaptureNativeAsync(FrameworkElement element, string name)
    {
        var bitmap = new RenderTargetBitmap();
        await bitmap.RenderAsync(element);
        var pixels = await bitmap.GetPixelsAsync();
        string path = Path.Combine(Path.GetTempPath(), $"kynxa-unknown-outcome-{name}.png");
        using var stream = new InMemoryRandomAccessStream();
        var encoder = await BitmapEncoder.CreateAsync(BitmapEncoder.PngEncoderId, stream);
        encoder.SetPixelData(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Premultiplied, (uint)bitmap.PixelWidth, (uint)bitmap.PixelHeight,
            96, 96, pixels.ToArray());
        await encoder.FlushAsync();
        stream.Seek(0);
        var bytes = new byte[stream.Size];
        await stream.ReadAsync(bytes.AsBuffer(), (uint)stream.Size, InputStreamOptions.None);
        File.WriteAllBytes(path, bytes);
        return path;
    }
}
