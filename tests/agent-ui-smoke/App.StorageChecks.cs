using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using MemoryUiSmoke;

namespace AgentUiSmoke;

public partial class App
{
    private async Task CheckStorageRowsAsync()
    {
        UiText.Initialize("zh-CN");
        string dataPath = Path.Combine(_directory, "Data"), extensionPath = Path.Combine(_directory, "Extensions");
        var data = new StorageLocationRow("数据存储", dataPath, "StorageDirectoryPath", "StorageDirectoryChooseButton");
        var extensions = new StorageLocationRow("工具与技能存储", extensionPath,
            "ExtensionStorageDirectoryPath", "ExtensionStorageDirectoryChooseButton");
        var status = new InfoBar { IsOpen = false, IsClosable = false };
        var content = new StackPanel { Margin = new Thickness(24), Spacing = 12 };
        content.Children.Add(data); content.Children.Add(extensions); content.Children.Add(status);
        var host = new Grid { RequestedTheme = ElementTheme.Light, Background = new SolidColorBrush(Microsoft.UI.Colors.White) };
        host.Children.Add(content);
        _anchor!.Content = host;
        _anchor.Title = "KYNXA · isolated storage rows";
        _anchor.AppWindow.Resize(new Windows.Graphics.SizeInt32(720, 420));
        _anchor.Activate();
        await SettleAsync();
        Check(NativeUi.IsVisible(data) && NativeUi.IsVisible(extensions) && !status.IsOpen,
            "storage rows show both current paths without an idle status notice");
        Check(data.Padding == extensions.Padding && data.CornerRadius == extensions.CornerRadius &&
            data.ChangeButton.Style == extensions.ChangeButton.Style && data.Background is SolidColorBrush dataFill &&
            extensions.Background is SolidColorBrush extensionFill && dataFill.Color == extensionFill.Color &&
            ReferenceEquals(data.Background, AppearanceService.GetBrush("KynxaSettingsCardBrush")) &&
            ReferenceEquals(extensions.Background, data.Background) &&
            dataFill.Color == AppearanceService.ParseColor(AppearanceService.Current.Sidebar),
            "data and extension rows share the current palette card brush, layout and quiet button style");
        Check(data.LocationLabel.Text == "数据存储" && extensions.LocationLabel.Text == "工具与技能存储" &&
            data.ChangeButton.Content?.ToString() == "更改位置" && extensions.ChangeButton.Content?.ToString() == "更改位置",
            "both storage choices use the same native button format");
        UiText.Initialize("en"); await SettleAsync();
        Check(data.LocationLabel.Text == "Data storage" && extensions.LocationLabel.Text == "Tool and skill storage" &&
            extensions.ChangeButton.Content?.ToString() == "Change location" && data.PathText.Text == dataPath && extensions.PathText.Text == extensionPath,
            "live storage row translation preserves the selected paths");
        var selection = new TaskCompletionSource<string?>(TaskCreationOptions.RunContinuationsAsynchronously);
        extensions.ChangeButton.Click += async (_, _) =>
        {
            data.ChangeButton.IsEnabled = extensions.ChangeButton.IsEnabled = false;
            var chosen = await selection.Task;
            if (chosen is not null)
            {
                extensions.SetPath(chosen);
                UiLocalization.Bind(status, InfoBar.MessageProperty, "工具与技能存储位置已更新，原文件已保留。");
                status.IsOpen = true;
                status.Severity = InfoBarSeverity.Success;
            }
            data.ChangeButton.IsEnabled = extensions.ChangeButton.IsEnabled = true;
        };
        NativeUi.Invoke(extensions.ChangeButton); await SettleAsync();
        Check(!data.ChangeButton.IsEnabled && !extensions.ChangeButton.IsEnabled,
            "an active extension folder selection disables both storage buttons");
        selection.SetResult(null); await SettleAsync();
        Check(extensions.PathText.Text == extensionPath && !status.IsOpen && data.ChangeButton.IsEnabled && extensions.ChangeButton.IsEnabled,
            "cancelled folder selection preserves the path and restores both controls");
        selection = new TaskCompletionSource<string?>(TaskCreationOptions.RunContinuationsAsynchronously);
        NativeUi.Invoke(extensions.ChangeButton);
        string selectedPath = Path.Combine(_directory, "Chosen", "Extensions");
        selection.SetResult(selectedPath); await SettleAsync();
        Check(extensions.PathText.Text == selectedPath && ToolTipService.GetToolTip(extensions.PathText)?.ToString() == selectedPath &&
            data.PathText.Text == dataPath && status.IsOpen && status.Severity == InfoBarSeverity.Success,
            "explicit fixture folder selection updates only extension storage and reports its result");
        UiText.Initialize("zh-CN"); await SettleAsync();
        Check(extensions.LocationLabel.Text == "工具与技能存储" && status.Message == "工具与技能存储位置已更新，原文件已保留。" &&
            extensions.PathText.Text == selectedPath, "completed storage status localizes without changing paths");
        extensions.ChangeButton.Focus(FocusState.Keyboard); await SettleAsync();
        await NativeWindowCapture.CaptureAsync(_anchor, Path.Combine(_directory, "storage-rows-zh.png"));
        _anchor.AppWindow.Hide();
        _window!.Activate();
    }
}
