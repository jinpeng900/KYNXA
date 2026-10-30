using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    internal void ShowMainWindowCloseFeedback(string key)
    {
        if (!_projectViewClosed && !_chatClosing) ShowActionFeedback(UiText.Get(key));
    }

    internal async Task<string?> ConfirmMainWindowCloseAsync()
    {
        if (_projectViewClosed || _chatClosing || _savingOnClose || XamlRoot is null) return null;
        var preferences = GetMainWindowClosePreferences();
        if (!preferences.Confirm) return preferences.Action;

        var content = new StackPanel { Spacing = 16, MinWidth = 0, MaxWidth = 420 };
        var description = new TextBlock { TextWrapping = TextWrapping.Wrap,
            Foreground = AppearanceService.GetBrush("KynxaSecondaryTextBrush") };
        UiLocalization.Bind(description, TextBlock.TextProperty, "请选择点击关闭按钮后的操作。");
        content.Children.Add(description);
        content.Children.Add(BuildMainWindowCloseOptions(preferences.Action, "MainWindowCloseDialog", out var tray, out var exit));
        var doNotAsk = new CheckBox();
        UiLocalization.Bind(doNotAsk, ContentControl.ContentProperty, "不再询问");
        AutomationProperties.SetAutomationId(doNotAsk, "MainWindowCloseDoNotAsk");
        content.Children.Add(doNotAsk);
        var error = new InfoBar { IsOpen = false, IsClosable = false, Severity = InfoBarSeverity.Error };
        UiLocalization.Bind(error, InfoBar.MessageProperty, "关闭偏好未能保存，请稍后重试。");
        AutomationProperties.SetAutomationId(error, "MainWindowCloseSaveError");
        AutomationProperties.SetLiveSetting(error, Microsoft.UI.Xaml.Automation.Peers.AutomationLiveSetting.Polite);
        content.Children.Add(error);
        var dialog = new ContentDialog { XamlRoot = XamlRoot, Content = content, DefaultButton = ContentDialogButton.Primary };
        UiLocalization.Bind(dialog, ContentDialog.TitleProperty, "关闭主窗口");
        UiLocalization.Bind(dialog, ContentDialog.PrimaryButtonTextProperty, "确定");
        UiLocalization.Bind(dialog, ContentDialog.CloseButtonTextProperty, "取消");
        AutomationProperties.SetAutomationId(dialog, "MainWindowCloseDialog");
        string? confirmedAction = null;
        dialog.PrimaryButtonClick += (_, args) =>
        {
            if (_projectViewClosed || _chatClosing || _savingOnClose) { args.Cancel = true; return; }
            string action = exit.IsChecked == true ? MainWindowClosePreferences.Exit : MainWindowClosePreferences.Tray;
            // Cancellation never changes preferences; a rejected preference save keeps both the dialog and window open.
            // 取消不修改偏好；偏好保存被拒绝时，保留确认框与窗口，不执行关闭动作。
            if (!TrySetMainWindowClosePreferences(new(action, doNotAsk.IsChecked != true)))
            {
                args.Cancel = true;
                error.IsOpen = true;
                return;
            }
            confirmedAction = action;
        };
        try
        {
            return await DialogPresentation.ShowAsync(dialog) == ContentDialogResult.Primary &&
                !_projectViewClosed && !_chatClosing && !_savingOnClose ? confirmedAction : null;
        }
        catch (System.Runtime.InteropServices.COMException)
        {
            // A different modal may already own the root; keep the main window available for a later close attempt.
            // 其他模态窗口可能已占用此可视根；保留主窗口，允许用户处理后再次关闭。
            ShowMainWindowCloseFeedback("暂时无法显示关闭确认，请先关闭其他弹窗。");
            return null;
        }
    }
}
