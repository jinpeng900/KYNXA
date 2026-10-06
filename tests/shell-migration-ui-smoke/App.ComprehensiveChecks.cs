using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop;

public partial class App
{
    private async Task CheckComprehensiveShellAsync()
    {
        var layout = ShellField<LayoutState>("_layout");
        var originalModel = ShellField<ModelChoice>("_selectedModel");
        string originalPermission = layout.PermissionMode;
        string draft = Prompt.Text;
        Guid? chatId = ActiveId;
        try
        {
            // A synthetic label exercises layout without changing available models or saving model preferences.
            // 虚构长标签仅检验布局，不修改模型列表或保存模型偏好。
            SetShellField("_selectedModel", new ModelChoice("fixture-provider", "Fixture provider",
                "fixture-model-" + new string('M', 180)));
            Call("UpdateModelPickerLabel");
            ResizeInDips(480, 540);
            UiText.Initialize("en");
            await SettleAsync();
            NativeUi.Invoke(Element<Button>("PermissionPickerButton"));
            await WaitAsync(() => HasPopup<Button>("PermissionOption_" + ChatPermissionModes.Full),
                "the real compact permission menu opens");
            var full = Popup<Button>("PermissionOption_" + ChatPermissionModes.Full);
            Check(PopupRoot(full).ActualWidth <= _shell.XamlRoot.Size.Width - 30 &&
                new[] { ChatPermissionModes.Ask, ChatPermissionModes.Smart, ChatPermissionModes.Full }
                    .All(mode => IsInsidePopup(Popup<Button>("PermissionOption_" + mode))),
                "English permission descriptions and all three actions fit in the real narrow popup");
            NativeUi.Invoke(full);
            await SettleAsync();
            var composer = Element<ComposerSurface>("ComposerHost");
            var permission = Element<Button>("PermissionPickerButton");
            var model = Element<Button>("ModelPickerButton");
            var send = Element<Button>("SendButton");
            Windows.Foundation.Rect Bounds(FrameworkElement control) => control.TransformToVisual(composer)
                .TransformBounds(new Windows.Foundation.Rect(0, 0, control.ActualWidth, control.ActualHeight));
            Check(Element<TextBlock>("SelectedPermissionLabel").Visibility == Visibility.Collapsed &&
                AutomationProperties.GetName(permission).Contains(UiText.Get("完全访问权限")) &&
                ToolTipService.GetToolTip(permission)?.ToString()?.Contains(UiText.Get("完全访问权限")) == true,
                "the compact permission icon retains its complete localized name and explanatory tooltip");
            Check(Bounds(permission).Right <= Bounds(model).Left + 1 && Bounds(model).Right <= Bounds(send).Left + 1 &&
                Bounds(send).Right <= composer.ActualWidth + 1 && model.ActualWidth > 30,
                "480 DIP Shell keeps a long model name, full permission and Send in separate visible regions");
            Check(Prompt.Text == draft && ActiveId == chatId && _gateway.Writes == 0,
                "compact permission presentation preserves the original draft and conversation without gateway writes");
            await CapturePresentedAsync("shell-comprehensive-long-model-480-en.png");
            NativeUi.Invoke(Element<Button>("CompactSidebarButton"));
            await SettleAsync();
            Check(NativeUi.IsVisible(Element<Button>("SidebarScrim")) && (bool)Call("TryDismissCompactSidebar")! &&
                !NativeUi.IsVisible(Element<Button>("SidebarScrim")) &&
                Element<Button>("CompactSidebarButton").FocusState != FocusState.Unfocused,
                "the Escape handler's production dismissal returns focus from the temporary drawer without saving its state");
        }
        finally
        {
            SetShellField("_selectedModel", originalModel);
            layout.PermissionMode = originalPermission;
            UiText.Initialize("zh-CN");
            Call("UpdateModelPickerLabel");
            Call("UpdatePermissionPickerLabel");
            ResizeInDips(1440, 900);
            Call("ApplyLayout");
            Call("SaveLayout");
            await SettleAsync();
        }

        Window? settings = null;
        try
        {
            UiText.Initialize("en");
            Call("StorageSettings_Click", _shell, new RoutedEventArgs());
            settings = ShellField<Window>("_storageSettingsWindow");
            var root = (FrameworkElement)settings.Content;
            await WaitAsync(() => root.IsLoaded && root.XamlRoot is not null, "the production settings window is loaded");
            await SettleAsync();
            double scale = root.XamlRoot.RasterizationScale;
            Check(settings.AppWindow.Size.Width / scale > 600 && settings.AppWindow.Size.Height / scale > 500,
                "settings opens at a readable DIP size rather than becoming tiny on a scaled display");
            settings.AppWindow.Resize(new Windows.Graphics.SizeInt32((int)(400 * scale), (int)(360 * scale)));
            await SettleAsync();
            var rows = NativeUi.Descendants<StorageLocationRow>(root).ToArray();
            Check(rows.Length == 2 && rows.All(row => Grid.GetRow(row.PathText) == 1 && row.PathText.IsTextSelectionEnabled &&
                ToolTipService.GetToolTip(row.PathText)?.ToString() == row.PathText.Text),
                "narrow settings places both full selectable storage paths below their labels with exact path tooltips");
            Check(rows.All(row => row.ChangeButton.TransformToVisual(row).TransformBounds(new Windows.Foundation.Rect(
                0, 0, row.ChangeButton.ActualWidth, row.ChangeButton.ActualHeight)).Right <= row.ActualWidth + 1),
                "English storage actions fit inside each compact row");
            rows[^1].PathText.StartBringIntoView();
            await SettleAsync();
            await NativeWindowCapture.CaptureAsync(settings, Path.Combine(_directory, "shell-comprehensive-settings-400-en.png"));
            settings.AppWindow.Resize(new Windows.Graphics.SizeInt32((int)(660 * scale), (int)(560 * scale)));
            await SettleAsync();
            Check(rows.All(row => Grid.GetRow(row.PathText) == 0) && Prompt.Text == draft && ActiveId == chatId && _gateway.Writes == 0,
                "widening settings restores one-line paths and keeps the Shell context unchanged");
            await NativeWindowCapture.CaptureAsync(settings, Path.Combine(_directory, "shell-comprehensive-settings-wide-en.png"));
        }
        finally
        {
            settings?.Close();
            UiText.Initialize("zh-CN");
            await SettleAsync();
        }
    }
}
