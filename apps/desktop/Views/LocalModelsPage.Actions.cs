using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Storage;
using Windows.Storage.Pickers;

namespace KYNXA_Desktop.Views;

public sealed partial class LocalModelsPage
{
    private async void ModelAction_Click(object sender, RoutedEventArgs args)
    {
        if (sender is not Button { Tag: string id } button || !_rows.TryGetValue(id, out var row)) return;
        switch (button.Name)
        {
            case "LocalModelInstall":
                await RunOperationAsync(() => _api.InstallAsync(id, _lifetime.Token), id);
                break;
            case "LocalModelPause":
                await RunOperationAsync(() => _api.DownloadAsync(id, "pause", _lifetime.Token), id);
                break;
            case "LocalModelResume":
                await RunOperationAsync(() => _api.DownloadAsync(id, "resume", _lifetime.Token), id);
                break;
            case "LocalModelCancel":
                await RunOperationAsync(() => _api.DownloadAsync(id, "cancel", _lifetime.Token), id);
                break;
            case "LocalModelEnable":
                await RunOperationAsync(() => _api.EnableAsync(id, !row.Model.Enabled, _lifetime.Token), id);
                break;
            case "LocalModelDelete":
                await RunOperationAsync(() => _api.DeleteAsync(id, _lifetime.Token), id);
                break;
        }
    }

    private ContentDialog Dialog(string title, object content, string primary)
    {
        var dialog = new ContentDialog
        {
            XamlRoot = _root.XamlRoot,
            Title = UiText.Get(title),
            Content = content,
            PrimaryButtonText = UiText.Get(primary),
            CloseButtonText = UiText.Get("取消"),
            DefaultButton = ContentDialogButton.None,
            PrimaryButtonStyle = (Style)Application.Current.Resources["KynxaQuietButtonStyle"],
            CloseButtonStyle = (Style)Application.Current.Resources["KynxaQuietButtonStyle"]
        };
        ApplyNeutralResources(dialog.Resources);
        return dialog;
    }

    private async Task ChangeDirectoryAsync()
    {
        if (_closed || _settingsBusy || _snapshot is null) return;
        try
        {
            var picker = new FolderPicker { SuggestedStartLocation = PickerLocationId.DocumentsLibrary };
            picker.FileTypeFilter.Add("*");
            WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(_owner));
            var folder = await picker.PickSingleFolderAsync();
            if (_closed || folder is null || string.Equals(folder.Path, _snapshot.InstallDirectory, StringComparison.OrdinalIgnoreCase)) return;
            var content = new StackPanel { Spacing = 10 };
            content.Children.Add(Label("当前目录", 12, secondary: true));
            content.Children.Add(new TextBlock { Text = _snapshot.InstallDirectory, FontFamily = UIFont, IsTextSelectionEnabled = true, TextWrapping = TextWrapping.Wrap });
            content.Children.Add(Label("新目录", 12, secondary: true));
            content.Children.Add(new TextBlock { Text = folder.Path, FontFamily = UIFont, IsTextSelectionEnabled = true, TextWrapping = TextWrapping.Wrap });
            var move = new CheckBox { FontFamily = UIFont, IsEnabled = _snapshot.Models.Any(model => model.Installed),
                Content = UiText.Get("同时移动已安装的受管模型（引用文件保持原位）") };
            content.Children.Add(move);
            content.Children.Add(Label("不勾选时仅改变以后安装的位置；已有模型继续按原路径使用。", 12, secondary: true));
            if (await Dialog("更改模型安装目录", content, "应用目录").ShowAsync() != ContentDialogResult.Primary) return;
            await RunOperationAsync(() => _api.SettingsAsync(installDirectory: folder.Path, moveExisting: move.IsChecked == true,
                token: _lifetime.Token), settings: true);
        }
        catch (Exception error) { ShowError(error); }
    }

    private async Task OpenDirectoryAsync()
    {
        if (_closed || _settingsBusy || string.IsNullOrWhiteSpace(_snapshot?.InstallDirectory)) return;
        string directory = _snapshot.InstallDirectory;
        await RunOperationAsync(async () =>
        {
            // Let the owner create/check the library before the first installation as well.
            // 首次安装前也由后端所有者创建并检查模型目录，界面只负责打开。
            var snapshot = await _api.SettingsAsync(installDirectory: directory, token: _lifetime.Token);
            if (!_closed)
            {
                var folder = await StorageFolder.GetFolderFromPathAsync(snapshot.InstallDirectory);
                await Windows.System.Launcher.LaunchFolderAsync(folder);
            }
            return snapshot;
        }, settings: true);
    }

    private async Task<StorageFile?> PickGgufAsync()
    {
        var picker = new FileOpenPicker { SuggestedStartLocation = PickerLocationId.Downloads };
        picker.FileTypeFilter.Add(".gguf");
        WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(_owner));
        return await picker.PickSingleFileAsync();
    }

    private async Task ImportModelAsync()
    {
        if (_closed || _settingsBusy) return;
        try
        {
            var file = await PickGgufAsync();
            if (_closed || file is null) return;
            var name = new TextBox { Text = Path.GetFileNameWithoutExtension(file.Name), FontFamily = UIFont, CornerRadius = new CornerRadius(8),
                Header = UiText.Get("模型名称"), MaxLength = 256 };
            var copy = new CheckBox { FontFamily = UIFont, Content = UiText.Get("复制到模型目录（不勾选则引用原文件）") };
            var projector = new CheckBox { FontFamily = UIFont, Content = UiText.Get("另外选择视觉投影文件（mmproj）") };
            var content = new StackPanel { Spacing = 10 };
            content.Children.Add(new TextBlock { Text = file.Path, FontFamily = UIFont, IsTextSelectionEnabled = true, TextWrapping = TextWrapping.Wrap });
            content.Children.Add(name);
            content.Children.Add(copy);
            content.Children.Add(projector);
            content.Children.Add(Label("只有带兼容视觉投影的模型才可理解图片；导入后仍需启用模型。", 12, secondary: true));
            if (await Dialog("导入本地 GGUF", content, "导入").ShowAsync() != ContentDialogResult.Primary) return;
            string? mmprojPath = null;
            if (projector.IsChecked == true)
            {
                var selected = await PickGgufAsync();
                if (_closed || selected is null) return;
                mmprojPath = selected.Path;
            }
            await RunOperationAsync(() => _api.ImportAsync(file.Path, name.Text.Trim(), copy.IsChecked == true,
                mmprojPath, _lifetime.Token), settings: true);
        }
        catch (Exception error) { ShowError(error); }
    }
}
