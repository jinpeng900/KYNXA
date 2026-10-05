using System.Net.Http.Json;
using System.Text.Json;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Storage.Pickers;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private sealed record ExtensionStorageHealth(int ExtensionStorageProtocol, string? ExtensionRoot, int ActiveRequests, bool Migrating,
        bool MigrationReady = false, string? RuntimeCleanupError = null);

    private sealed class StorageSettingsControls(Window window, StorageLocationRow dataRow, StorageLocationRow extensionRow,
        Button memoryButton, Button toolsButton, ComboBox languagePicker, ProgressRing progress, InfoBar status)
    {
        private readonly CancellationTokenSource _lifetime = new();
        public Window Window => window;
        public StorageLocationRow ExtensionRow => extensionRow;
        public CancellationToken Token => IsClosed ? new CancellationToken(canceled: true) : _lifetime.Token;
        public bool IsBusy { get; private set; }
        public bool IsClosed { get; private set; }

        public void SetBusy(bool busy)
        {
            IsBusy = busy;
            if (IsClosed) return;
            dataRow.ChangeButton.IsEnabled = !busy && !StoragePaths.EnvironmentControlled;
            extensionRow.ChangeButton.IsEnabled = !busy && !ExtensionPaths.EnvironmentControlled;
            memoryButton.IsEnabled = toolsButton.IsEnabled = languagePicker.IsEnabled = !busy;
        }

        public void ShowProgress(bool active)
        { if (!IsClosed) { progress.IsActive = active; progress.Visibility = active ? Visibility.Visible : Visibility.Collapsed; } }

        public void Message(string text, InfoBarSeverity severity = InfoBarSeverity.Informational)
        { if (!IsClosed) { UiLocalization.Bind(status, InfoBar.MessageProperty, text); status.Severity = severity; status.IsOpen = true; } }

        public void Close()
        { IsClosed = true; _lifetime.Cancel(); _lifetime.Dispose(); }
    }

    private static void RequireExtensionStorageHealth(ExtensionStorageHealth? health, string expectedRoot)
    {
        if (health?.ExtensionStorageProtocol != 1 || string.IsNullOrWhiteSpace(health.ExtensionRoot) || !Path.IsPathFullyQualified(health.ExtensionRoot))
            throw new InvalidOperationException(UiText.Get("当前网关版本不支持工具与技能存储迁移，请关闭旧网关并重新打开 KYNXA。"));
        if (!string.Equals(Path.TrimEndingDirectorySeparator(Path.GetFullPath(health.ExtensionRoot)),
            Path.TrimEndingDirectorySeparator(Path.GetFullPath(expectedRoot)), StringComparison.OrdinalIgnoreCase))
            throw new InvalidOperationException(UiText.Get("桌面与网关的工具与技能目录不一致，请统一启动配置后再迁移。"));
    }

    private async Task ChangeExtensionStorageAsync(StorageSettingsControls controls)
    {
        if (StoragePaths.IsMigrating || controls.IsBusy || controls.IsClosed) return;
        controls.SetBusy(true);
        FileStream? maintenance = null;
        bool acquired = false, moved = false, migrationStarted = false;
        string? destination = null;
        try
        {
            if (ExtensionPaths.EnvironmentControlled)
                throw new InvalidOperationException(UiText.Get("启动环境变量正在指定工具与技能目录。"));
            if (_toolManagementWindow is { HasPendingChanges: true })
                throw new InvalidOperationException(UiText.Get("请先完成工具配置，再更改工具与技能存储位置。"));
            _toolManagementWindow?.CloseForOwner();
            if (_sendingPrompt || _pendingReplies.Values.Any(reply => reply.Error is null))
                throw new InvalidOperationException(UiText.Get("请等待模型回复完成后再迁移。"));
            using (var self = System.Diagnostics.Process.GetCurrentProcess())
            {
                var instances = System.Diagnostics.Process.GetProcessesByName(self.ProcessName);
                bool another = instances.Any(instance => instance.Id != self.Id);
                foreach (var instance in instances) instance.Dispose();
                if (another) throw new InvalidOperationException(UiText.Get("请先关闭其他 KYNXA 实例，再迁移数据。"));
            }
            var address = ModelGatewayService.Address;
            if (address.Scheme != "http" || address.Host is not ("localhost" or "127.0.0.1"))
                throw new InvalidOperationException(UiText.Get("此设置需要连接本机模型网关。"));
            var picker = new FolderPicker { SuggestedStartLocation = PickerLocationId.ComputerFolder };
            picker.FileTypeFilter.Add("*");
            WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(controls.Window));
            var folder = await picker.PickSingleFolderAsync();
            if (folder is null || controls.IsClosed || _storageSettingsWindow != controls.Window) return;
            if (_sendingPrompt || _pendingReplies.Values.Any(reply => reply.Error is null))
                throw new InvalidOperationException(UiText.Get("请等待模型回复完成后再迁移。"));
            string source = ExtensionPaths.Root;
            destination = Path.TrimEndingDirectorySeparator(Path.GetFullPath(folder.Path));
            if (string.Equals(destination, Path.TrimEndingDirectorySeparator(source), StringComparison.OrdinalIgnoreCase))
            { controls.Message("已经在使用这个目录。", InfoBarSeverity.Success); return; }
            StoragePaths.IsMigrating = true;
            migrationStarted = true;
            IsEnabled = false;
            controls.ShowProgress(true);
            controls.Message("正在准备迁移，请保持应用打开…");
            await ModelGatewayService.EnsureReadyAsync(controls.Token);
            using var http = new HttpClient { BaseAddress = address, Timeout = TimeSpan.FromSeconds(5) };
            var health = await http.GetFromJsonAsync<ExtensionStorageHealth>("/health", controls.Token);
            RequireExtensionStorageHealth(health, source);
            Directory.CreateDirectory(Path.GetDirectoryName(StoragePaths.MigrationLockPath)!);
            maintenance = new FileStream(StoragePaths.MigrationLockPath, FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.Read);
            acquired = true;
            maintenance.SetLength(0);
            await maintenance.WriteAsync(JsonSerializer.SerializeToUtf8Bytes(new { pid = Environment.ProcessId }), controls.Token);
            maintenance.Flush(true);
            var deadline = DateTime.UtcNow.AddSeconds(30);
            do
            {
                health = await http.GetFromJsonAsync<ExtensionStorageHealth>("/health", controls.Token);
                RequireExtensionStorageHealth(health, source);
                if (health!.RuntimeCleanupError is not null) throw new InvalidOperationException(UiText.Get("后台工具未安全停止，请重启网关后再迁移。"));
                if (health.Migrating && health.ActiveRequests == 0 && health.MigrationReady) break;
                if (DateTime.UtcNow >= deadline)
                    throw new InvalidOperationException(UiText.Get("模型网关还有请求正在处理，请稍后再试。"));
                await Task.Delay(250, controls.Token);
            } while (true);
            controls.Message("正在迁移，请保持应用打开…");
            var updates = new Progress<string>(text => { if (!moved) controls.Message(text); });
            var result = await Task.Run(() => ExtensionStorageMigrationService.MoveAsync(source, destination, ExtensionPaths.PointerPath,
                updates, controls.Token), controls.Token);
            moved = true;
            ExtensionPaths.Reload();
            controls.ExtensionRow.SetPath(result.ExtensionRoot);
        }
        catch (OperationCanceledException) when (controls.IsClosed) { }
        catch (Exception error)
        {
            controls.Message(moved ? UiText.Get("工具与技能位置已更新，但设置刷新失败：") + error.Message
                : error.Message + UiText.Get(" 若目标目录已生成部分副本，请选择另一个空目录重试。"), InfoBarSeverity.Error);
        }
        finally
        {
            maintenance?.Dispose();
            if (acquired) { try { File.Delete(StoragePaths.MigrationLockPath); } catch (Exception error) when (error is IOException or UnauthorizedAccessException) { } }
            if (migrationStarted) { StoragePaths.IsMigrating = false; IsEnabled = !moved; }
            if (!moved) { controls.ShowProgress(false); controls.SetBusy(false); }
        }
        if (!moved) return;
        if (controls.IsClosed) { IsEnabled = true; return; }
        try
        {
            using var http = new HttpClient { BaseAddress = ModelGatewayService.Address, Timeout = TimeSpan.FromSeconds(5) };
            var health = await http.GetFromJsonAsync<ExtensionStorageHealth>("/health", controls.Token);
            RequireExtensionStorageHealth(health, destination!);
            if (health!.Migrating) throw new InvalidOperationException(UiText.Get("网关仍处于维护状态，请稍后重试。"));
            controls.Message("工具与技能存储位置已更新，原文件已保留。", InfoBarSeverity.Success);
        }
        catch (OperationCanceledException) when (controls.IsClosed) { }
        catch (Exception error)
        { controls.Message(UiText.Get("位置已更新，但网关尚未加载新的工具与技能目录：") + error.Message, InfoBarSeverity.Error); }
        finally { IsEnabled = true; controls.ShowProgress(false); controls.SetBusy(false); }
    }
}
