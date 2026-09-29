using System.Net.Http.Json;
using System.Text.Json;
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
    private Window? _storageSettingsWindow;
    private sealed record StorageHealth(int StorageProtocol, int ActiveRequests, bool Migrating, string? ModelDataHome);

    private void StorageSettings_Click(object sender, RoutedEventArgs args)
    {
        if (!_projectsReady) return;
        if (_storageSettingsWindow is { } existing)
        {
            if (existing.AppWindow.Presenter is Microsoft.UI.Windowing.OverlappedPresenter presenter) presenter.Restore();
            existing.Activate();
            return;
        }
        var window = new Window { Title = "KYNXA · 设置" };
        _storageSettingsWindow = window;
        var font = (FontFamily)Application.Current.Resources["KynxaUIFont"];
        var content = new StackPanel { Spacing = 12, Margin = new Thickness(24, 24, 24, 24) };
        TextBlock Label(string text, double size = 14) => new() { Text = text, FontSize = size, FontFamily = font, TextWrapping = TextWrapping.Wrap };
        var section = Label("存储", 13);
        section.Opacity = 0.6;
        content.Children.Add(section);
        var row = new Grid { ColumnSpacing = 14, Padding = new Thickness(14, 12, 14, 12), CornerRadius = new CornerRadius(12),
            Background = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 247, 247, 247)) };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var label = Label("数据存储");
        label.VerticalAlignment = VerticalAlignment.Center;
        var current = Label(StoragePaths.DataRoot ?? StoragePaths.DesktopDirectory, 13);
        current.TextWrapping = TextWrapping.NoWrap;
        current.TextTrimming = TextTrimming.CharacterEllipsis;
        current.VerticalAlignment = VerticalAlignment.Center;
        current.Opacity = 0.7;
        ToolTipService.SetToolTip(current, current.Text);
        AutomationProperties.SetAutomationId(current, "StorageDirectoryPath");
        var choose = new Button { Content = "更改位置", FontFamily = font, FontSize = 13, Padding = new Thickness(10, 5, 10, 5), VerticalAlignment = VerticalAlignment.Center };
        Grid.SetColumn(current, 1); Grid.SetColumn(choose, 2);
        row.Children.Add(label); row.Children.Add(current); row.Children.Add(choose);
        content.Children.Add(row);
        var progress = new ProgressRing { IsActive = false, Width = 24, Height = 24, Visibility = Visibility.Collapsed };
        var status = new InfoBar { IsOpen = false, IsClosable = false };
        content.Children.Add(progress); content.Children.Add(status);
        var root = new Grid { RequestedTheme = ElementTheme.Light, Background = new SolidColorBrush(Microsoft.UI.Colors.White) };
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        var titleBar = new Grid { Height = 48, Background = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 250, 249, 248)) };
        var title = Label("KYNXA  /  设置");
        title.Margin = new Thickness(20, 0, 140, 0);
        title.VerticalAlignment = VerticalAlignment.Center;
        titleBar.Children.Add(title);
        var scroll = new ScrollViewer { Content = content, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        Grid.SetRow(scroll, 1);
        root.Children.Add(titleBar); root.Children.Add(scroll);
        window.Content = root;
        window.ExtendsContentIntoTitleBar = true;
        window.SetTitleBar(titleBar);
        window.AppWindow.Resize(new Windows.Graphics.SizeInt32(720, 420));
        var ownerBounds = App.Window.AppWindow;
        window.AppWindow.Move(new Windows.Graphics.PointInt32(ownerBounds.Position.X + Math.Max(0, (ownerBounds.Size.Width - 720) / 2),
            ownerBounds.Position.Y + Math.Max(0, (ownerBounds.Size.Height - 420) / 2)));
        if (Microsoft.UI.Windowing.AppWindowTitleBar.IsCustomizationSupported())
        {
            window.AppWindow.TitleBar.ButtonBackgroundColor = Microsoft.UI.Colors.Transparent;
            window.AppWindow.TitleBar.ButtonInactiveBackgroundColor = Microsoft.UI.Colors.Transparent;
            window.AppWindow.TitleBar.ButtonForegroundColor = Microsoft.UI.Colors.Black;
        }
        void Message(string text, InfoBarSeverity severity = InfoBarSeverity.Informational)
        { status.Message = text; status.Severity = severity; status.IsOpen = true; }
        choose.IsEnabled = !StoragePaths.EnvironmentControlled;
        if (StoragePaths.EnvironmentControlled) Message("数据目录由启动环境变量指定，请移除 KYNXA_DATA_HOME / KYNXA_MODEL_HOME 后再通过设置修改。", InfoBarSeverity.Warning);
        void PreventClose(Microsoft.UI.Windowing.AppWindow window, Microsoft.UI.Windowing.AppWindowClosingEventArgs e)
        { if (StoragePaths.IsMigrating) e.Cancel = true; }
        App.Window.AppWindow.Closing += PreventClose;
        window.AppWindow.Closing += PreventClose;
        void CloseSettings(object sender, WindowEventArgs e) => window.Close();
        App.Window.Closed += CloseSettings;
        window.Closed += (_, _) =>
        {
            App.Window.AppWindow.Closing -= PreventClose;
            App.Window.Closed -= CloseSettings;
            _storageSettingsWindow = null;
        };
        choose.Click += async (_, _) =>
        {
            if (StoragePaths.IsMigrating) return;
            choose.IsEnabled = false;
            FileStream? maintenance = null;
            bool acquired = false;
            try
            {
                if (StoragePaths.EnvironmentControlled) throw new InvalidOperationException("启动环境变量正在指定数据目录。");
                if (_sendingPrompt || _pendingReplies.Values.Any(reply => reply.Error is null)) throw new InvalidOperationException("请等待模型回复完成后再迁移。");
                using (var self = System.Diagnostics.Process.GetCurrentProcess())
                {
                    var instances = System.Diagnostics.Process.GetProcessesByName(self.ProcessName);
                    bool another = instances.Any(instance => instance.Id != self.Id);
                    foreach (var instance in instances) instance.Dispose();
                    if (another) throw new InvalidOperationException("请先关闭其他 KYNXA 实例，再迁移数据。");
                }
                var address = ModelGatewayService.Address;
                if (address.Scheme != "http" || address.Host is not ("localhost" or "127.0.0.1")) throw new InvalidOperationException("此设置需要连接本机模型网关。");
                var picker = new FolderPicker { SuggestedStartLocation = PickerLocationId.ComputerFolder };
                picker.FileTypeFilter.Add("*");
                WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(window));
                var folder = await picker.PickSingleFolderAsync();
                if (folder is null) return;
                if (_storageSettingsWindow != window) return;
                if (_sendingPrompt || _pendingReplies.Values.Any(reply => reply.Error is null)) throw new InvalidOperationException("请等待模型回复完成后再迁移。");
                string destination = Path.TrimEndingDirectorySeparator(Path.GetFullPath(folder.Path));
                if (StoragePaths.DataRoot is string currentRoot && string.Equals(destination, Path.TrimEndingDirectorySeparator(currentRoot), StringComparison.OrdinalIgnoreCase))
                { Message("已经在使用这个目录。", InfoBarSeverity.Success); return; }
                CaptureProjectDraft(); CaptureStandaloneDraft();
                _projectStore.Save(_projects); _projectStore.SaveChats(_standaloneChats);
                _layoutStateService.Save(_layout);
                StoragePaths.IsMigrating = true;
                IsEnabled = false;
                progress.IsActive = true; progress.Visibility = Visibility.Visible;
                Message("正在准备迁移，请保持应用打开…");
                await ModelGatewayService.EnsureReadyAsync();
                using var http = new HttpClient { BaseAddress = address, Timeout = TimeSpan.FromSeconds(5) };
                var health = await http.GetFromJsonAsync<StorageHealth>("/health");
                if (health?.StorageProtocol != 1 || string.IsNullOrWhiteSpace(health.ModelDataHome)) throw new InvalidOperationException("当前网关版本不支持存储迁移，请关闭旧网关并重新打开 KYNXA。");
                string expectedModels = StoragePaths.DataRoot is string dataRoot ? Path.Combine(dataRoot, "Models")
                    : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".kynxa", "models");
                if (!string.Equals(Path.TrimEndingDirectorySeparator(Path.GetFullPath(health.ModelDataHome)),
                    Path.TrimEndingDirectorySeparator(Path.GetFullPath(expectedModels)), StringComparison.OrdinalIgnoreCase))
                    throw new InvalidOperationException("桌面与网关的数据目录不一致，请统一启动配置后再迁移。");
                Directory.CreateDirectory(Path.GetDirectoryName(StoragePaths.MigrationLockPath)!);
                maintenance = new FileStream(StoragePaths.MigrationLockPath, FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.Read);
                acquired = true;
                maintenance.SetLength(0);
                await maintenance.WriteAsync(JsonSerializer.SerializeToUtf8Bytes(new { pid = Environment.ProcessId }));
                maintenance.Flush(true);
                health = await http.GetFromJsonAsync<StorageHealth>("/health");
                if (health is null || !health.Migrating || health.ActiveRequests != 0) throw new InvalidOperationException("模型网关还有请求正在处理，请稍后再试。");
                Message("正在迁移，请保持应用打开…");
                string sourceDesktop = StoragePaths.DesktopDirectory, sourceModels = health.ModelDataHome!;
                var updates = new Progress<string>(text => Message(text));
                var result = await Task.Run(() => StorageMigrationService.MoveAsync(sourceDesktop, sourceModels, destination, StoragePaths.PointerPath, updates));
                StoragePaths.Reload();
                _projectStore = new ProjectStore(StoragePaths.DesktopDirectory);
                _modelSelectionStore = new ModelSelectionStore(StoragePaths.DesktopDirectory);
                foreach (var project in _projects)
                    if (project.FolderPath is string oldPath && StorageMigrationService.IsWithin(oldPath, Path.Combine(sourceDesktop, "Projects")))
                        project.FolderPath = Path.Combine(StoragePaths.DesktopDirectory, Path.GetRelativePath(sourceDesktop, oldPath));
                current.Text = result.DataRoot;
                ToolTipService.SetToolTip(current, result.DataRoot);
                Message("存储位置已更新，原数据已保留。", InfoBarSeverity.Success);
            }
            catch (Exception error) { Message(error.Message + " 若目标目录已生成部分副本，请选择另一个空目录重试。", InfoBarSeverity.Error); }
            finally
            {
                maintenance?.Dispose();
                if (acquired) { try { File.Delete(StoragePaths.MigrationLockPath); } catch (IOException) { } }
                StoragePaths.IsMigrating = false;
                IsEnabled = true;
                choose.IsEnabled = !StoragePaths.EnvironmentControlled;
                progress.IsActive = false; progress.Visibility = Visibility.Collapsed;
            }
        };
        window.Activate();
    }
}
