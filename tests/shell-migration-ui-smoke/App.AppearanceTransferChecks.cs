using System.Text;
using System.Text.Json;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Windows.Storage;

namespace KYNXA_Desktop;

public partial class App
{
    private async Task CheckAppearanceTransferAsync()
    {
        string layoutPath = Path.Combine(DesktopDirectory, "layout.json");
        string originalLayout = File.ReadAllText(layoutPath);
        var original = JsonSerializer.Deserialize<LayoutState>(originalLayout)!;
        var expected = new AppearanceTransferPreferences(original.AppearancePaletteId, original.AppearanceMode,
            original.ChatFontSize, original.AppearanceAccent);
        string originalLanguage = UiText.Language, originalDraft = Prompt.Text;
        string messages = JsonSerializer.Serialize(_shell.ActiveMessages.Select(row => row.Message));
        Guid? activeId = ActiveId;
        Window? settings = null;
        int pickerCalls = 0;
        try
        {
            NativeUi.SetText(Prompt, "APPEARANCE_TRANSFER_UNSENT 中文");
            var imported = new AppearanceTransferPreferences("violet", "system", 18.5, "#AB12CD");
            byte[] validBytes = AppearancePreferencesTransfer.Serialize(imported);
            string validJson = Encoding.UTF8.GetString(validBytes);
            Check(AppearancePreferencesTransfer.Parse(validBytes) == imported,
                "versioned appearance JSON preserves preset, system mode, half-pixel font and custom HEX exactly");
            Check(AppearancePreferencesTransfer.Parse(new byte[] { 0xEF, 0xBB, 0xBF }.Concat(validBytes).ToArray()) == imported,
                "appearance JSON accepts a Windows editor UTF-8 BOM");
            Check(AppearancePreferencesTransfer.Parse(AppearancePreferencesTransfer.Serialize(imported with { CustomAccent = null })).CustomAccent is null,
                "a null custom accent round-trips as the preset color policy");
            string[] invalidDocuments =
            [
                "", "[]", "{", "null",
                validJson.Replace("\"version\": 1", "\"version\": 2"),
                validJson.Replace("\"version\": 1", "\"version\": \"1\""),
                validJson.Replace("kynxa.appearance", "another.format"),
                validJson.Replace("\"violet\"", "\"future-palette\""),
                validJson.Replace("\"system\"", "\"future-mode\""),
                validJson.Replace("18.5", "12.5"), validJson.Replace("18.5", "22.5"),
                validJson.Replace("18.5", "18.25"), validJson.Replace("18.5", "\"18.5\""),
                validJson.Replace("#AB12CD", "#GGGGGG"), validJson.Replace("#AB12CD", " #AB12CD "),
                validJson.Replace("\"mode\":", "\"missingMode\":"),
                validJson.Replace("\"mode\": \"system\"", "\"mode\": \"system\", \"mode\": \"dark\""),
                validJson.Replace("\"mode\": \"system\"", "\"mode\": \"system\", \"apiKey\": \"fixture-only\""),
                new string(' ', AppearancePreferencesTransfer.MaximumFileBytes + 1)
            ];
            for (int index = 0; index < invalidDocuments.Length; index++)
            {
                bool rejected = false;
                try { AppearancePreferencesTransfer.Parse(Encoding.UTF8.GetBytes(invalidDocuments[index])); }
                catch (Exception error) when (error is InvalidDataException or JsonException or InvalidOperationException) { rejected = true; }
                Check(rejected, "appearance parser rejects invalid shape/version/type/value/duplicate/unknown/oversize case " + index);
            }

            string importPath = Path.Combine(_directory, "import-appearance.json");
            string exportPath = Path.Combine(_directory, "export-appearance.json");
            File.WriteAllBytes(importPath, validBytes);
            File.WriteAllText(exportPath, "fixture export placeholder");
            var importFile = await StorageFile.GetFileFromPathAsync(importPath);
            var exportFile = await StorageFile.GetFileFromPathAsync(exportPath);
            Call("StorageSettings_Click", _shell, new RoutedEventArgs());
            settings = ShellField<Window>("_storageSettingsWindow");
            var root = (FrameworkElement)settings.Content;
            await WaitAsync(() => root.IsLoaded && root.XamlRoot is not null, "production appearance transfer settings window loads");
            var importButton = NativeUi.ById<Button>(root, "AppearanceImportButton");
            var exportButton = NativeUi.ById<Button>(root, "AppearanceExportButton");
            var status = NativeUi.ById<InfoBar>(root, "AppearanceSettingsStatus");
            var accentInput = NativeUi.ById<TextBox>(root, "AppearanceAccentInput");
            NativeUi.SetText(accentInput, "#C01234");
            _shell.AppearanceExportPickerOverride = () => { pickerCalls++; return Task.FromResult<StorageFile?>(exportFile); };
            async Task InvokeTransferAsync(Button button)
            {
                int before = pickerCalls;
                NativeUi.Invoke(button);
                await WaitAsync(() => pickerCalls > before && importButton.IsEnabled && exportButton.IsEnabled,
                    "production appearance transfer completes and releases both action buttons");
            }
            await InvokeTransferAsync(exportButton);
            Check(AppearancePreferencesTransfer.Parse(File.ReadAllBytes(exportPath)) == expected && accentInput.Text == "#C01234"
                && File.ReadAllText(layoutPath) == originalLayout,
                "real StorageFile export contains saved preferences and excludes an unapplied HEX draft without saving the layout");
            using (var document = JsonDocument.Parse(File.ReadAllBytes(exportPath)))
                Check(document.RootElement.EnumerateObject().Select(property => property.Name).Order().SequenceEqual(
                    new[] { "format", "version", "paletteId", "mode", "chatFontSize", "customAccent" }.Order()),
                    "export has exactly the six allowed fields and excludes chats, paths, language, credentials and effective system colors");
            Check(status.IsOpen && status.Severity == InfoBarSeverity.Success && status.Message == UiText.Get("外观已导出。"),
                "export success appears only in the local appearance status");
            UiText.Initialize("en");
            await SettleAsync();
            Check(importButton.Content?.ToString() == "Import appearance" && exportButton.Content?.ToString() == "Export appearance"
                && status.Message == "Appearance exported." && accentInput.Text == "#C01234",
                "language switching translates transfer controls and local feedback while preserving the pending HEX");
            UiText.Initialize("zh-CN");

            string exported = File.ReadAllText(exportPath);
            _shell.AppearanceImportPickerOverride = () => { pickerCalls++; return Task.FromResult<StorageFile?>(null); };
            _shell.AppearanceExportPickerOverride = () => { pickerCalls++; return Task.FromResult<StorageFile?>(null); };
            await InvokeTransferAsync(importButton);
            await InvokeTransferAsync(exportButton);
            Check(File.ReadAllText(layoutPath) == originalLayout && File.ReadAllText(exportPath) == exported
                && accentInput.Text == "#C01234" && status.Severity == InfoBarSeverity.Success,
                "cancelled import/export preserves saved appearance, pending HEX, export file and prior local status");
            _shell.AppearanceImportPickerOverride = () => { pickerCalls++; return Task.FromResult<StorageFile?>(importFile); };
            File.WriteAllText(importPath, invalidDocuments[4]);
            await InvokeTransferAsync(importButton);
            Check(status.Severity == InfoBarSeverity.Error && File.ReadAllText(layoutPath) == originalLayout
                && AppearanceService.Current.Id == expected.PaletteId && accentInput.Text == "#C01234",
                "invalid-version import reports a local error without partial save, runtime change or loss of the HEX draft");
            File.WriteAllBytes(importPath, validBytes);
            string blockedTemporaryPath = layoutPath + ".tmp";
            Directory.CreateDirectory(blockedTemporaryPath);
            try
            {
                await InvokeTransferAsync(importButton);
                Check(status.Severity == InfoBarSeverity.Error && File.ReadAllText(layoutPath) == originalLayout
                    && AppearanceService.Current.Id == expected.PaletteId && accentInput.Text == "#C01234"
                    && ShellField<LayoutState>("_layout").AppearancePaletteId == expected.PaletteId,
                    "an actual atomic-layout-save failure rolls back the imported fields before changing runtime colors or the HEX draft");
            }
            finally { Directory.Delete(blockedTemporaryPath); }
            await InvokeTransferAsync(importButton);
            var saved = new LayoutStateService().Load();
            Check(saved.AppearancePaletteId == imported.PaletteId && saved.AppearanceMode == imported.Mode
                && saved.ChatFontSize == imported.ChatFontSize && saved.AppearanceAccent == imported.CustomAccent
                && AppearanceService.Current.Id == imported.PaletteId && AppearanceService.Mode == imported.Mode
                && AppearanceService.ChatFontSize == imported.ChatFontSize && AppearanceService.CustomAccent == imported.CustomAccent
                && accentInput.Text == imported.CustomAccent && status.Severity == InfoBarSeverity.Success,
                "valid StorageFile import atomically persists all four fields and then synchronizes native runtime and the input controls");
            Check(saved.SidebarWidth == original.SidebarWidth && saved.InterfaceLanguage == original.InterfaceLanguage
                && saved.PermissionMode == original.PermissionMode && Prompt.Text == "APPEARANCE_TRANSFER_UNSENT 中文"
                && ActiveId == activeId && JsonSerializer.Serialize(_shell.ActiveMessages.Select(row => row.Message)) == messages && _gateway.Writes == 0,
                "appearance import preserves unrelated layout, permissions, language, chat, unsent text and formal gateway state");
            _shell.AppearanceExportPickerOverride = () => { pickerCalls++; throw new IOException("fixture-only selected-file failure"); };
            await InvokeTransferAsync(exportButton);
            Check(status.Severity == InfoBarSeverity.Error && status.Message == UiText.Get("外观导出失败，请检查所选位置后重试。")
                && AppearanceService.Current.Id == imported.PaletteId,
                "selected-file/export failure has local error feedback and preserves the applied appearance");

            double scale = root.XamlRoot.RasterizationScale;
            settings.AppWindow.Resize(new Windows.Graphics.SizeInt32((int)Math.Ceiling(400 * scale), (int)Math.Ceiling(920 * scale)));
            await SettleAsync();
            Check(importButton.ActualWidth > 100 && exportButton.ActualWidth > 100,
                "appearance transfer actions remain usable in a compact settings window");
            await InvokeTransferAsync(exportButton);
            await SettleAsync();
            var statusBounds = status.TransformToVisual(root).TransformBounds(new Windows.Foundation.Rect(0, 0, status.ActualWidth, status.ActualHeight));
            Check(statusBounds.Top >= 0 && statusBounds.Bottom <= root.ActualHeight + 1,
                "explicit transfer feedback scrolls into the visible compact settings viewport");
            await NativeWindowCapture.CaptureAsync(settings, Path.Combine(_directory, "shell-appearance-transfer.png"));
            var pendingPicker = new TaskCompletionSource<StorageFile?>(TaskCreationOptions.RunContinuationsAsynchronously);
            _shell.AppearanceImportPickerOverride = () => { pickerCalls++; return pendingPicker.Task; };
            NativeUi.Invoke(importButton);
            await WaitAsync(() => !importButton.IsEnabled && !exportButton.IsEnabled,
                "one pending appearance picker disables both transfer actions");
            string savedBeforeClose = File.ReadAllText(layoutPath);
            settings.Close();
            settings = null;
            pendingPicker.SetResult(importFile);
            await SettleAsync();
            Check(File.ReadAllText(layoutPath) == savedBeforeClose && _unhandled is null,
                "closing settings discards a late file selection without import, save or a detached-window exception");
            File.WriteAllText(Path.Combine(_directory, "appearance-transfer-export.json"), exported);
        }
        finally
        {
            _shell.AppearanceImportPickerOverride = null;
            _shell.AppearanceExportPickerOverride = null;
            settings?.Close();
            var layout = ShellField<LayoutState>("_layout");
            layout.AppearancePaletteId = original.AppearancePaletteId;
            layout.AppearanceMode = original.AppearanceMode;
            layout.ChatFontSize = original.ChatFontSize;
            layout.AppearanceAccent = original.AppearanceAccent;
            File.WriteAllText(layoutPath, originalLayout);
            AppearanceService.ApplyPreferences(expected.PaletteId, expected.Mode, expected.ChatFontSize, expected.CustomAccent);
            UiText.Initialize(originalLanguage);
            NativeUi.SetText(Prompt, originalDraft);
        }
    }
}
