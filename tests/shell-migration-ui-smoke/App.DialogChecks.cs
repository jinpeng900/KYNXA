using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;

namespace KYNXA_Desktop;

public partial class App
{
    private readonly List<object> _dialogActionBounds = [];

    private void CheckDialogActionLayout(ContentDialog dialog, Border background, string label)
    {
        var command = NativeUi.ByName<Grid>(dialog, "CommandSpace");
        var acknowledgementTemplate = (DataTemplate)Current.Resources["KynxaDialogAcknowledgementTemplate"];
        var actions = command.Children.OfType<Button>().Where(button => button.Visibility == Visibility.Visible)
            .Select(button =>
            {
                var presenter = NativeUi.ByName<ContentPresenter>(button, "ContentPresenter");
                var text = NativeUi.Descendants<TextBlock>(presenter).FirstOrDefault(block => block.Text == button.Content?.ToString());
                return new
                {
                    Button = button, Presenter = presenter, Text = text,
                    TextBounds = text?.TransformToVisual(button).TransformBounds(new Rect(0, 0, text.ActualWidth, text.ActualHeight)),
                    Bounds = button.TransformToVisual(background).TransformBounds(new Rect(0, 0, button.ActualWidth, button.ActualHeight))
                };
            }).OrderBy(action => action.Bounds.Left).ToArray();
        _dialogActionBounds.Add(new
        {
            Label = label, BackgroundWidth = background.ActualWidth,
            Actions = actions.Select(action => new
            {
                action.Button.Name, Text = action.Button.Content?.ToString(), action.Bounds.X, action.Bounds.Y,
                action.Bounds.Width, action.Bounds.Height,
                action.Button.MinWidth, action.Button.MinHeight, action.Button.Padding,
                MaxWidth = double.IsFinite(action.Button.MaxWidth) ? action.Button.MaxWidth : (double?)null,
                MaxWidthIsUnbounded = double.IsPositiveInfinity(action.Button.MaxWidth),
                ButtonCharacterSpacing = action.Button.CharacterSpacing,
                PresenterCharacterSpacing = action.Presenter.CharacterSpacing,
                TextCharacterSpacing = action.Text?.CharacterSpacing,
                UsesAcknowledgementTemplate = ReferenceEquals(action.Button.ContentTemplate, acknowledgementTemplate),
                ContentTemplateIsNull = action.Button.ContentTemplate is null,
                TextHasLocalSpacing = action.Text is not null && action.Text.ReadLocalValue(TextBlock.CharacterSpacingProperty) != DependencyProperty.UnsetValue,
                TextFontSize = action.Text?.FontSize, TextFontFamily = action.Text?.FontFamily.Source,
                TextIsTrimmed = action.Text?.IsTextTrimmed,
                TextDesiredWidth = action.Text?.DesiredSize.Width,
                TextX = action.TextBounds?.X, TextY = action.TextBounds?.Y,
                TextWidth = action.TextBounds?.Width, TextHeight = action.TextBounds?.Height,
                CenterOffset = action.Bounds.X + action.Bounds.Width / 2 - background.ActualWidth / 2
            }).ToArray()
        });
        File.WriteAllText(Path.Combine(_directory, "shell-dialog-action-layout.json"),
            JsonSerializer.Serialize(_dialogActionBounds, new JsonSerializerOptions { WriteIndented = true }));
        foreach (var action in actions)
        {
            int expectedSpacing = actions.Length == 1 && action.Button.Content?.ToString() == "知道了" ? 180 : 0;
            Check(action.Text is not null && action.Button.CharacterSpacing == expectedSpacing &&
                action.Presenter.CharacterSpacing == expectedSpacing && action.Text.CharacterSpacing == expectedSpacing,
                label + " retains original text and native character spacing through the button, presenter and rendered text");
            if (expectedSpacing == 180)
            {
                // Check initial product rendering before any diagnostic can opt the SDK text node out of its fast path.
                // 在任何诊断改变快速文字路径之前验证产品首次渲染，测试不能通过本地设置字距替产品修正排版。
                Check(ReferenceEquals(action.Button.ContentTemplate, acknowledgementTemplate) &&
                    action.Text!.ReadLocalValue(TextBlock.CharacterSpacingProperty) is int localSpacing && localSpacing == 180 &&
                    action.Text.ActualWidth >= action.Text.FontSize * 3 + 4 && action.Text.Text == "知道了",
                    label + " initially renders the unchanged acknowledgement with explicit local tracking and visibly expanded text width");
            }
            else
            {
                Check(action.Button.ContentTemplate is null,
                    label + " preserves the original native content template for other labels and multiple actions");
            }
            if (actions.Length == 1)
            {
                var textBounds = action.TextBounds!.Value;
                Check(action.Button.Padding.Equals(new Thickness(24, 6, 24, 6)) &&
                    !action.Text!.IsTextTrimmed && textBounds.Width > 0 && textBounds.Height > 0 &&
                    action.Text.DesiredSize.Width <= textBounds.Width + 1 &&
                    textBounds.Left >= action.Button.Padding.Left - 1 &&
                    textBounds.Right <= action.Button.ActualWidth - action.Button.Padding.Right + 1 &&
                    Math.Abs(textBounds.X + textBounds.Width / 2 - action.Button.ActualWidth / 2) <= 1,
                    label + " keeps symmetric padding and centered untrimmed text inside the actual single action");
            }
        }
        if (actions.Length == 1)
        {
            var bounds = actions[0].Bounds;
            Check(bounds.Width >= 129 && bounds.Width <= 203 && bounds.Height >= 35 && bounds.Height <= 44 &&
                bounds.Left >= 0 && bounds.Right <= background.ActualWidth &&
                Math.Abs(bounds.X + bounds.Width / 2 - background.ActualWidth / 2) <= 1,
                label + " centers its sole bounded action symmetrically within the actual dialog surface");
        }
        else
        {
            Check(actions.Length >= 2 && actions.All(action => action.Bounds.Width > 0 && action.Bounds.Left >= 0 &&
                action.Bounds.Right <= background.ActualWidth) &&
                Enumerable.Range(1, actions.Length - 1).All(index => actions[index].Bounds.Left - actions[index - 1].Bounds.Right >= 4) &&
                Math.Abs((actions[0].Bounds.Left + actions[^1].Bounds.Right) / 2 - background.ActualWidth / 2) <= 1,
                label + " retains separately spaced native actions across the symmetric command area");
        }
    }

    private async Task CheckLongSingleActionAsync(ContentDialog dialog)
    {
        var button = NativeUi.ByName<Button>(dialog, "CloseButton");
        string original = dialog.CloseButtonText;
        double shortWidth = button.ActualWidth;
        const string LongLabel = "返回查看详细说明";
        try
        {
            // Reuse the same production modal and restore its label; this does not create another product action.
            // 复用同一个生产弹窗并恢复原标签，不新增产品操作或修改本地化字符串。
            dialog.CloseButtonText = LongLabel;
            await WaitAsync(() => button.Content?.ToString() == LongLabel && button.CharacterSpacing == 0,
                "the reused production action applies the long label with its original native character spacing");
            await SettleAsync();
            CheckDialogSurface(dialog, "Adaptive long Chinese action");
            Check(button.ActualWidth > shortWidth + 8 && button.ActualWidth <= 203 &&
                button.Content?.ToString() == LongLabel && dialog.CloseButtonText == LongLabel,
                "the same native single action grows for a longer exact label within the 202 DIP cap");
            await CapturePresentedAsync("shell-dialog-schedule-long-action-zh.png");
        }
        finally
        {
            dialog.CloseButtonText = original;
            await WaitAsync(() => button.Content?.ToString() == original && button.CharacterSpacing == 180,
                "restoring the exact Chinese acknowledgement restores its native spacing");
            await SettleAsync();
        }
        Check(dialog.CloseButtonText == "知道了" && button.Content?.ToString() == "知道了" &&
            Math.Abs(button.ActualWidth - shortWidth) <= 1,
            "the original three-character string and compact centered button size return without inserted spaces");
        CheckDialogSurface(dialog, "Restored Chinese acknowledgement");
    }

    private void CheckDialogSurface(ContentDialog dialog, string label)
    {
        var background = NativeUi.ByName<Border>(dialog, "BackgroundElement");
        var inner = NativeUi.ByName<Grid>(dialog, "DialogSpace");
        Check(dialog.IsLoaded && dialog.XamlRoot is not null && background.ActualWidth > 0 &&
            dialog.RequestedTheme == (AppearanceService.Current.IsDark ? ElementTheme.Dark : ElementTheme.Light) &&
            dialog.ActualTheme == dialog.RequestedTheme,
            label + " uses its native popup and the current owner theme");
        Check(ReferenceEquals(background.Background, AppearanceService.GetBrush("KynxaMainBrush")) &&
            ReferenceEquals(background.BorderBrush, AppearanceService.GetBrush("KynxaDividerBrush")) &&
            ReferenceEquals(dialog.Foreground, AppearanceService.GetBrush("KynxaTextBrush")) &&
            background.BorderThickness.Left == 1 && background.CornerRadius.TopLeft == 16 &&
            background.CornerRadius.BottomRight == 16 && inner.CornerRadius.TopLeft == 16 && inner.CornerRadius.BottomRight == 16,
            label + " renders the shared surface, text and border with matching outer and inner 16 DIP corners");
        CheckDialogActionLayout(dialog, background, label);
    }

    private async Task CheckDialogButtonStatesAsync(ContentDialog dialog, string buttonName, bool accent, string label)
    {
        var button = NativeUi.ByName<Button>(dialog, buttonName);
        var presenter = NativeUi.ByName<ContentPresenter>(button, "ContentPresenter");
        string foreground = accent ? "KynxaOnAccentBrush" : "KynxaTextBrush";
        foreach (var state in new[] { ("Normal", accent ? "KynxaSendBrush" : "KynxaSettingsCardBrush"),
            ("PointerOver", accent ? "KynxaSendHoverBrush" : "KynxaControlHoverBrush"),
            ("Pressed", accent ? "KynxaSendPressedBrush" : "KynxaControlPressedBrush") })
        {
            // Inspect the actual SDK presenter after changing its native state; no second button template is copied here.
            // 切换原生状态后读取真实 SDK 呈现层，不在测试中复制另一份按钮模板。
            Check(VisualStateManager.GoToState(button, state.Item1, false), label + " enters native " + state.Item1);
            await SettleAsync();
            Check(presenter.Background is SolidColorBrush fill && presenter.Foreground is SolidColorBrush text &&
                fill.Color == AppearanceService.GetBrush(state.Item2).Color && text.Color == AppearanceService.GetBrush(foreground).Color &&
                AppearanceService.ContrastRatio($"#{fill.Color.R:X2}{fill.Color.G:X2}{fill.Color.B:X2}",
                    $"#{text.Color.R:X2}{text.Color.G:X2}{text.Color.B:X2}") >= 4.5 &&
                presenter.CornerRadius.TopLeft == 8 && button.CornerRadius.BottomRight == 8,
                label + " keeps actual " + state.Item1 + " fill, text contrast and rounded button corners");
        }
        VisualStateManager.GoToState(button, "Normal", false);
    }

    private async Task CheckDialogsAsync()
    {
        string language = UiText.Language;
        var palette = AppearanceService.Current;
        string mode = AppearanceService.Mode;
        double fontSize = AppearanceService.ChatFontSize;
        string? custom = AppearanceService.CustomAccent;
        string draft = Prompt.Text;
        Guid? chatId = ActiveId;
        var messages = _shell.ActiveMessages.Select(row => row.Message).ToArray();
        int writes = _gateway.Writes;
        ContentDialog? current = null;
        try
        {
            await ActivateOwnedShortcutWindowAsync();
            AppearanceService.ApplyPreferences("mist-blue", "light", fontSize, null);
            UiText.Initialize("zh-CN");
            var schedule = NativeUi.Descendants<Button>(_shell).Single(button =>
                AutomationProperties.GetName(button) == "定时任务" && NativeUi.IsVisible(button));
            NativeUi.Invoke(schedule);
            await WaitAsync(() => NativeUi.OpenDialog(_shell)?.Title?.ToString() == "定时任务", "the real schedule entry opens its production information dialog");
            current = NativeUi.OpenDialog(_shell)!;
            await SettleAsync();
            CheckDialogSurface(current, "Light schedule");
            await CheckDialogButtonStatesAsync(current, "CloseButton", accent: true, "Light default close action");
            await CapturePresentedAsync("shell-dialog-schedule-light.png");
            Check(current.CloseButtonText == "知道了", "Chinese acknowledgement keeps the exact original three-character string");
            await CheckLongSingleActionAsync(current);
            var sameSchedule = current;
            AppearanceService.ApplyPreferences("teal", "dark", fontSize, null);
            await SettleAsync();
            Check(ReferenceEquals(NativeUi.OpenDialog(_shell), sameSchedule), "changing appearance preserves the same open native modal");
            CheckDialogSurface(current, "Dark schedule");
            await CheckDialogButtonStatesAsync(current, "CloseButton", accent: true, "Dark default close action");
            await CapturePresentedAsync("shell-dialog-schedule-dark.png");
            DispatchShortcut(0x1B, control: false);
            await WaitAsync(() => NativeUi.OpenDialog(_shell) is null, "actual Escape dismisses the schedule modal");
            await SettleAsync();
            var closedTheme = sameSchedule.RequestedTheme;
            AppearanceService.ApplyPreferences("violet", "light", fontSize, "#8055B4");
            await SettleAsync();
            Check(sameSchedule.RequestedTheme == closedTheme, "a closed modal no longer receives owner theme updates");
            current = null;

            DispatchShortcut(0x70, control: false);
            await WaitAsync(() => NativeUi.OpenDialog(_shell)?.Title?.ToString() == "键盘操作", "the production help shortcut opens keyboard instructions");
            current = NativeUi.OpenDialog(_shell)!;
            await SettleAsync();
            CheckDialogSurface(current, "Custom light keyboard help");
            await CapturePresentedAsync("shell-dialog-keyboard-custom-light.png");
            AppearanceService.ApplyPreferences("sand", "dark", fontSize, "#8055B4");
            await SettleAsync();
            CheckDialogSurface(current, "Custom dark keyboard help");
            await CheckDialogButtonStatesAsync(current, "CloseButton", accent: true, "Custom default close action");
            await CapturePresentedAsync("shell-dialog-keyboard-custom-dark.png");
            NativeUi.InvokeDialogButton(current, primary: false);
            await WaitAsync(() => NativeUi.OpenDialog(_shell) is null, "the visible help close button remains usable");
            current = null;
            await SettleAsync();

            foreach (string selectedLanguage in new[] { "zh-CN", "en" })
            {
                UiText.Initialize(selectedLanguage);
                var connections = NativeUi.Descendants<Button>(_shell).Single(button =>
                    AutomationProperties.GetName(button) == UiText.Get("连接") && NativeUi.IsVisible(button));
                NativeUi.Invoke(connections);
                await WaitAsync(() => NativeUi.OpenDialog(_shell)?.Title?.ToString() == UiText.Get("连接"),
                    selectedLanguage + " connections entry opens the feature placeholder");
                current = NativeUi.OpenDialog(_shell)!;
                await SettleAsync();
                Check(current.Content?.ToString() == UiText.Get("功能暂未开发") &&
                    typeof(Views.ShellPage).GetField("_toolManagementWindow", PrivateInstance)!.GetValue(_shell) is null,
                    selectedLanguage + " connections shows only the localized unavailable message and opens no tool management window");
                CheckDialogSurface(current, selectedLanguage + " connections");
                if (selectedLanguage == "en") await CapturePresentedAsync("shell-dialog-connections-en.png");
                NativeUi.InvokeDialogButton(current, primary: false);
                await WaitAsync(() => NativeUi.OpenDialog(_shell) is null, "connections close dismisses its native popup");
                current = null;
                await SettleAsync();
            }

            var englishSchedule = NativeUi.Descendants<Button>(_shell).Single(button =>
                AutomationProperties.GetName(button) == UiText.Get("定时任务") && NativeUi.IsVisible(button));
            NativeUi.Invoke(englishSchedule);
            await WaitAsync(() => NativeUi.OpenDialog(_shell)?.Title?.ToString() == UiText.Get("定时任务"),
                "the English schedule entry opens its production information dialog");
            current = NativeUi.OpenDialog(_shell)!;
            await SettleAsync();
            CheckDialogSurface(current, "English schedule");
            Check(current.CloseButtonText == UiText.Get("知道了") && current.CloseButtonText == "OK",
                "the English acknowledgement preserves its original localized string without added character spacing");
            await CapturePresentedAsync("shell-dialog-schedule-en.png");
            NativeUi.InvokeDialogButton(current, primary: false);
            await WaitAsync(() => NativeUi.OpenDialog(_shell) is null, "the centered English schedule action dismisses its dialog");
            current = null;
            await SettleAsync();

            DispatchShortcut(0x70, control: false);
            await WaitAsync(() => NativeUi.OpenDialog(_shell)?.Title?.ToString() == UiText.Get("键盘操作"),
                "actual F1 opens English keyboard instructions");
            current = NativeUi.OpenDialog(_shell)!;
            await SettleAsync();
            CheckDialogSurface(current, "English keyboard help");
            await CapturePresentedAsync("shell-dialog-keyboard-en.png");
            NativeUi.InvokeDialogButton(current, primary: false);
            await WaitAsync(() => NativeUi.OpenDialog(_shell) is null, "the centered English help action dismisses its dialog");
            current = null;
            await SettleAsync();

            var projectError = (Task)Call("ShowProjectErrorAsync", UiText.Get("项目操作未完成"), "Synthetic presentation-only error")!;
            await WaitAsync(() => NativeUi.OpenDialog(_shell) is not null, "the production project error opens its single-action dialog");
            current = NativeUi.OpenDialog(_shell)!;
            await SettleAsync();
            CheckDialogSurface(current, "Project error");
            Check(current.DefaultButton == ContentDialogButton.None, "centering a single project error action retains its original default decision");
            NativeUi.InvokeDialogButton(current, primary: false);
            await projectError;
            current = null;
            await SettleAsync();

            var rename = (Task<string?>)Call("AskChatNameAsync", "Synthetic preserved name")!;
            await WaitAsync(() => NativeUi.OpenDialog(_shell) is not null, "the production rename dialog opens");
            current = NativeUi.OpenDialog(_shell)!;
            await SettleAsync();
            var name = NativeUi.ById<TextBox>(current, "ChatNameInput");
            NativeUi.SetText(name, " ");
            // Native Value automation can return before WinUI dispatches the production TextChanged handler.
            // 原生 Value 自动化返回时，WinUI 可能尚未派发生产 TextChanged 回调；等待真实校验完成。
            await WaitAsync(() => name.Text == " " && !current.IsPrimaryButtonEnabled,
                "the production rename TextChanged handler disables an empty name");
            Check(!current.IsPrimaryButtonEnabled, "rename retains its actual empty-name validation");
            NativeUi.SetText(name, "Unsaved dialog fixture name");
            await WaitAsync(() => name.Text == "Unsaved dialog fixture name" && current.IsPrimaryButtonEnabled,
                "the production rename TextChanged handler enables a valid name");
            Check(current.IsPrimaryButtonEnabled && current.DefaultButton == ContentDialogButton.Primary,
                "rename retains its enabled save action and default primary semantics");
            CheckDialogSurface(current, "Rename");
            await CheckDialogButtonStatesAsync(current, "PrimaryButton", accent: true, "Rename primary");
            await CheckDialogButtonStatesAsync(current, "CloseButton", accent: false, "Rename cancel");
            await CapturePresentedAsync("shell-dialog-rename-dark.png");
            NativeUi.InvokeDialogButton(current, primary: false);
            Check(await rename is null, "canceling the production rename returns no replacement name");
            current = null;
            await SettleAsync();

            var tool = new ToolActivity("dialog-appearance-fixture", "workspace.read_file", null, "approval-required",
                "Read only an isolated synthetic file", ApprovalId: Guid.NewGuid(), WorkspaceRoot: Path.Combine(_directory, "Workspace"));
            current = ToolApprovalDialog.Create(_shell.XamlRoot, tool);
            var decision = current.ShowAsync();
            await WaitAsync(() => ReferenceEquals(NativeUi.OpenDialog(_shell), current), "the production approval factory opens on the same owner root");
            await SettleAsync();
            Check(current.DefaultButton == ContentDialogButton.None && current.XamlRoot == _shell.XamlRoot,
                "approval retains explicit decision semantics with no default approval");
            CheckDialogSurface(current, "Approval");
            await CheckDialogButtonStatesAsync(current, "PrimaryButton", accent: false, "Approval explicit primary");
            await CheckDialogButtonStatesAsync(current, "CloseButton", accent: false, "Approval reject");
            NativeUi.InvokeDialogButton(current, primary: false);
            Check(await decision == ContentDialogResult.None, "the actual approval reject button returns denial");
            current = null;
            await SettleAsync();

            await CheckRetrievalLinkDialogAsync();
            Check(Prompt.Text == draft && ActiveId == chatId && _shell.ActiveMessages.Select(row => row.Message).SequenceEqual(messages) &&
                _gateway.Writes == writes && _gateway.Failure is null && _unhandled is null,
                "feature, rename, approval and retrieval modals preserve draft, conversation, original message objects and formal data");
        }
        finally
        {
            current?.Hide();
            if (CurrentRetrievalWindow is { } retrieval) retrieval.CloseForOwner();
            AppearanceService.ApplyPreferences(palette.Id, mode, fontSize, custom);
            UiText.Initialize(language);
            await SettleAsync();
        }
    }

    private async Task CheckRetrievalLinkDialogAsync()
    {
        var knowledge = NativeUi.Descendants<Button>(_shell).Single(button =>
            AutomationProperties.GetName(button) == UiText.Get("知识库") && NativeUi.IsVisible(button));
        NativeUi.Invoke(knowledge);
        await WaitAsync(() => CurrentRetrievalWindow?.Content is FrameworkElement { IsLoaded: true } && !CurrentRetrievalWindow.HasPendingChanges,
            "the production retrieval owner window is loaded before opening its dialog");
        var retrieval = CurrentRetrievalWindow!;
        var root = (FrameworkElement)retrieval.Content;
        await WaitAsync(() => NativeUi.ById<Button>(root, "RetrievalAddLinks").IsEnabled, "the retrieval link button becomes available after its reads");
        NativeUi.Invoke(NativeUi.ById<Button>(root, "RetrievalAddLinks"));
        await WaitAsync(() => NativeUi.OpenDialog(root) is not null, "the actual retrieval add-links action opens its modal");
        var dialog = NativeUi.OpenDialog(root)!;
        await SettleAsync();
        CheckDialogSurface(dialog, "Retrieval links");
        Check(dialog.XamlRoot == root.XamlRoot && dialog.XamlRoot != _shell.XamlRoot,
            "the retrieval modal retains its actual secondary-window XamlRoot");
        NativeUi.InvokeDialogButton(dialog, primary: true);
        await SettleAsync();
        Check(ReferenceEquals(NativeUi.OpenDialog(root), dialog) && NativeUi.Descendants<TextBlock>(dialog)
            .Any(text => text.Text == UiText.Get("请输入 1～64 个链接。")),
            "the original link validation cancels an empty import without writing anything");
        AppearanceService.ApplyPreferences("teal", "light", AppearanceService.ChatFontSize, "#287A68");
        await SettleAsync();
        CheckDialogSurface(dialog, "Retrieval links after live theme change");
        NativeUi.InvokeDialogButton(dialog, primary: false);
        await WaitAsync(() => NativeUi.OpenDialog(root) is null && !retrieval.HasPendingChanges,
            "canceling link import completes the original pending operation");
        retrieval.CloseForOwner();
        await WaitAsync(() => CurrentRetrievalWindow is null, "the fixture-owned retrieval window closes cleanly");
        await ActivateOwnedShortcutWindowAsync();
    }
}
