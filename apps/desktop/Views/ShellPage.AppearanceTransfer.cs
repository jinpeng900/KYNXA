using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Storage;
using Windows.Storage.Pickers;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    internal Func<Task<StorageFile?>>? AppearanceImportPickerOverride { get; set; }
    internal Func<Task<StorageFile?>>? AppearanceExportPickerOverride { get; set; }

    private async Task<StorageFile?> PickAppearanceFileAsync(Window owner, bool isImport)
    {
        var pickerOverride = isImport ? AppearanceImportPickerOverride : AppearanceExportPickerOverride;
        if (pickerOverride is not null) return await pickerOverride();
        nint ownerWindow = WinRT.Interop.WindowNative.GetWindowHandle(owner);
        if (isImport)
        {
            var picker = new FileOpenPicker { SuggestedStartLocation = PickerLocationId.DocumentsLibrary };
            picker.FileTypeFilter.Add(".json");
            WinRT.Interop.InitializeWithWindow.Initialize(picker, ownerWindow);
            return await picker.PickSingleFileAsync();
        }
        else
        {
            var picker = new FileSavePicker
            {
                SuggestedStartLocation = PickerLocationId.DocumentsLibrary,
                SuggestedFileName = "KYNXA-appearance"
            };
            picker.FileTypeChoices.Add("JSON", [".json"]);
            WinRT.Interop.InitializeWithWindow.Initialize(picker, ownerWindow);
            return await picker.PickSaveFileAsync();
        }
    }

    private sealed partial class AppearanceSettingsSection
    {
        private Button _importAppearance = null!;
        private Button _exportAppearance = null!;
        private bool _transferBusy;

        private void AddTransferControls(FontFamily font, Func<AppearanceTransferPreferences> readSavedPreferences,
            Func<bool, Task<StorageFile?>> pickFile)
        {
            var transferGroup = new StackPanel { Spacing = 10, Padding = new Thickness(16, 14, 16, 14) };
            var actions = new Grid { ColumnSpacing = 8, MaxWidth = 460,
                HorizontalAlignment = HorizontalAlignment.Left, Width = 340 };
            actions.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            actions.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            Button Action(string key, string id)
            {
                var button = BuildAppearanceActionButton(font, key, id);
                button.HorizontalAlignment = HorizontalAlignment.Stretch;
                return button;
            }
            _importAppearance = Action("导入外观", "AppearanceImportButton");
            _exportAppearance = Action("导出外观", "AppearanceExportButton");
            Grid.SetColumn(_exportAppearance, 1);
            actions.Children.Add(_importAppearance);
            actions.Children.Add(_exportAppearance);
            transferGroup.Children.Add(actions);
            var hint = new TextBlock { FontFamily = font, FontSize = 12, TextWrapping = TextWrapping.Wrap,
                Foreground = AppearanceService.GetBrush("KynxaSecondaryTextBrush") };
            UiLocalization.Bind(hint, TextBlock.TextProperty, "仅包含已保存的配色、主题、聊天字号和强调色。导入会替换当前外观。");
            transferGroup.Children.Add(hint);
            Root.Children.Add(BuildSettingGroup(transferGroup));
            transferGroup.SizeChanged += (_, args) => actions.Width = Math.Min(340, Math.Max(0, args.NewSize.Width - 32));
            _importAppearance.Click += async (_, _) => await TransferAsync(true, readSavedPreferences, pickFile);
            _exportAppearance.Click += async (_, _) => await TransferAsync(false, readSavedPreferences, pickFile);
        }

        private async Task TransferAsync(bool isImport, Func<AppearanceTransferPreferences> readSavedPreferences,
            Func<bool, Task<StorageFile?>> pickFile)
        {
            if (_isDisposed || _transferBusy) return;
            _transferBusy = true;
            _importAppearance.IsEnabled = _exportAppearance.IsEnabled = false;
            try
            {
                var file = await pickFile(isImport);
                if (file is null || _isDisposed) return;
                if (isImport)
                {
                    var preferences = await AppearancePreferencesTransfer.ReadAsync(file);
                    if (_isDisposed) return;
                    string? errorKey = _savePreferences(new(preferences.PaletteId, preferences.Mode,
                        preferences.ChatFontSize, preferences.CustomAccent));
                    if (errorKey is not null)
                    {
                        ShowTransferStatus(errorKey, InfoBarSeverity.Error);
                        return;
                    }
                    _accentDirty = false;
                    RefreshSelection(null, EventArgs.Empty);
                }
                else
                {
                    // Read the saved preferences after the picker returns; an unapplied input draft is never exported.
                    // 文件选择完成后读取已保存偏好，不能将输入框中尚未应用的草稿导出。
                    await AppearancePreferencesTransfer.WriteAsync(file, readSavedPreferences());
                }
                if (!_isDisposed) ShowTransferStatus(isImport ? "外观已导入并保存。" : "外观已导出。", InfoBarSeverity.Success);
            }
            catch (OperationCanceledException) { }
            catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException or ArgumentException
                or InvalidOperationException or NotSupportedException or System.Text.Json.JsonException
                or System.Runtime.InteropServices.COMException)
            {
                if (!_isDisposed) ShowTransferStatus(isImport
                    ? "外观导入失败。请使用有效的 KYNXA 外观 JSON 文件（版本 1，不超过 16 KB），当前外观已保留。"
                    : "外观导出失败，请检查所选位置后重试。", InfoBarSeverity.Error);
            }
            finally
            {
                _transferBusy = false;
                if (!_isDisposed) _importAppearance.IsEnabled = _exportAppearance.IsEnabled = true;
            }
        }

        private void ShowTransferStatus(string key, InfoBarSeverity severity)
        {
            UiLocalization.Bind(_status, InfoBar.MessageProperty, key);
            _status.Severity = severity;
            _status.IsOpen = true;
            // A result below the scroll fold must be visible after an explicit transfer action.
            // 显式导入或导出后的本地反馈可能位于滚动区域下方，需要让结果进入可见区域。
            Root.DispatcherQueue.TryEnqueue(() =>
            {
                if (_isDisposed || !_status.IsOpen) return;
                _status.UpdateLayout();
                _status.StartBringIntoView(new BringIntoViewOptions { AnimationDesired = false });
            });
        }
    }
}
