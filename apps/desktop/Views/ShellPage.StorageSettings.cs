using System.Net.Http.Json;
using System.Text.Json;
using KYNXA_Desktop.Controls;
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
    private sealed record StorageHealth(int StorageProtocol, int ActiveRequests, bool Migrating, string? ModelDataHome,
        bool MigrationReady = false, string? RuntimeCleanupError = null);

    private void StorageSettings_Click(object sender, RoutedEventArgs args)
    {
        if (!_projectsReady) return;
        if (_storageSettingsWindow is { } existing)
        {
            if (existing.AppWindow.Presenter is Microsoft.UI.Windowing.OverlappedPresenter presenter) presenter.Restore();
            existing.Activate();
            return;
        }
        Exception? extensionLocationError = null;
        try { ExtensionPaths.Reload(); }
        catch (Exception error) { extensionLocationError = error; }
        var window = new Window { Title = UiText.Get("KYNXA · 设置") };
        _storageSettingsWindow = window;
        var font = (FontFamily)Application.Current.Resources["KynxaUIFont"];
        var content = new StackPanel { Spacing = 12, Margin = new Thickness(24, 24, 24, 24) };
        TextBlock Label(string text, double size = 14) => new() { Text = text, FontSize = size, FontFamily = font, TextWrapping = TextWrapping.Wrap };
        TextBlock LocalizedLabel(string key, double size = 14)
        {
            var text = Label(string.Empty, size);
            UiLocalization.Bind(text, TextBlock.TextProperty, key);
            return text;
        }
        var general = LocalizedLabel("通用", 13);
        general.Opacity = 0.6;
        content.Children.Add(general);
        var languageRow = new Grid { ColumnSpacing = 20, Padding = new Thickness(14, 10, 14, 10), CornerRadius = new CornerRadius(12),
            Background = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 247, 247, 247)) };
        languageRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        languageRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var languageLabel = LocalizedLabel("界面语言");
        languageLabel.VerticalAlignment = VerticalAlignment.Center;
        var languagePicker = new ComboBox { FontFamily = font, FontSize = 13,
            MinWidth = 140, VerticalAlignment = VerticalAlignment.Center };
        var focusBrush = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 112, 112, 112));
        var selectedBrush = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 200, 200, 200));
        languagePicker.Resources["ComboBoxBackgroundBorderBrushFocused"] = focusBrush;
        languagePicker.Resources["SystemControlFocusVisualPrimaryBrush"] = focusBrush;
        languagePicker.Resources["ComboBoxItemPillFillBrush"] = focusBrush;
        foreach (string key in new[] { "ComboBoxItemBorderBrushSelected", "ComboBoxItemBorderBrushSelectedPointerOver", "ComboBoxItemBorderBrushSelectedPressed" })
            languagePicker.Resources[key] = selectedBrush;
        languagePicker.Items.Add(new ComboBoxItem { Content = "简体中文", Tag = "zh-CN" });
        languagePicker.Items.Add(new ComboBoxItem { Content = "English", Tag = "en" });
        languagePicker.SelectedIndex = _layout.InterfaceLanguage == "en" ? 1 : 0;
        UiLocalization.Bind(languagePicker, AutomationProperties.NameProperty, "界面语言");
        AutomationProperties.SetAutomationId(languagePicker, "InterfaceLanguagePicker");
        Grid.SetColumn(languagePicker, 1);
        languageRow.Children.Add(languageLabel); languageRow.Children.Add(languagePicker);
        content.Children.Add(languageRow);
        var memoryRow = new Grid { ColumnSpacing = 14, Padding = new Thickness(14, 10, 14, 10), CornerRadius = new CornerRadius(12),
            Background = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 247, 247, 247)) };
        memoryRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        memoryRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var memoryLabel = LocalizedLabel("记忆管理");
        memoryLabel.VerticalAlignment = VerticalAlignment.Center;
        var memoryButton = new Button { FontFamily = font, FontSize = 13, Padding = new Thickness(12, 6, 12, 6), CornerRadius = new CornerRadius(8),
            Style = (Style)Application.Current.Resources["KynxaQuietButtonStyle"] };
        UiLocalization.Bind(memoryButton, Button.ContentProperty, "打开");
        AutomationProperties.SetAutomationId(memoryButton, "MemoryManagementSettingsButton");
        Grid.SetColumn(memoryButton, 1);
        memoryRow.Children.Add(memoryLabel);
        memoryRow.Children.Add(memoryButton);
        content.Children.Add(memoryRow);
        var toolsRow = new Grid { ColumnSpacing = 14, Padding = new Thickness(14, 10, 14, 10), CornerRadius = new CornerRadius(12),
            Background = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 247, 247, 247)) };
        toolsRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        toolsRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var toolsLabel = LocalizedLabel("工具与技能");
        toolsLabel.VerticalAlignment = VerticalAlignment.Center;
        var toolsButton = new Button { FontFamily = font, FontSize = 13, Padding = new Thickness(12, 6, 12, 6), CornerRadius = new CornerRadius(8),
            Style = (Style)Application.Current.Resources["KynxaQuietButtonStyle"] };
        UiLocalization.Bind(toolsButton, Button.ContentProperty, "打开");
        AutomationProperties.SetAutomationId(toolsButton, "AgentToolsSettingsButton");
        Grid.SetColumn(toolsButton, 1);
        toolsRow.Children.Add(toolsLabel); toolsRow.Children.Add(toolsButton);
        content.Children.Add(toolsRow);
        toolsButton.Click += (_, _) => { if (!StoragePaths.IsMigrating) OpenAgentTools(); };
        var retrievalRow = new Grid { ColumnSpacing = 14, Padding = new Thickness(14, 10, 14, 10), CornerRadius = new CornerRadius(12),
            Background = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 247, 247, 247)) };
        retrievalRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        retrievalRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        retrievalRow.Children.Add(LocalizedLabel("检索与网页搜索"));
        var retrievalButton = new Button { FontFamily = font, FontSize = 13, Padding = new Thickness(12, 6, 12, 6), CornerRadius = new CornerRadius(8),
            Style = (Style)Application.Current.Resources["KynxaQuietButtonStyle"] };
        UiLocalization.Bind(retrievalButton, Button.ContentProperty, "管理");
        AutomationProperties.SetAutomationId(retrievalButton, "RetrievalSettingsButton");
        Grid.SetColumn(retrievalButton, 1);
        retrievalRow.Children.Add(retrievalButton);
        content.Children.Add(retrievalRow);
        retrievalButton.Click += (_, _) => OpenRetrievalSettings();
        var section = LocalizedLabel("存储", 13);
        section.Opacity = 0.6;
        content.Children.Add(section);
        var row = new StorageLocationRow("数据存储", StoragePaths.DataRoot ?? StoragePaths.DesktopDirectory,
            "StorageDirectoryPath", "StorageDirectoryChooseButton");
        var choose = row.ChangeButton;
        content.Children.Add(row);
        var extensionRow = new StorageLocationRow("用户工具", ExtensionPaths.Root,
            "ExtensionStorageDirectoryPath", "ExtensionStorageDirectoryChooseButton");
        content.Children.Add(extensionRow);
        var progress = new ProgressRing { IsActive = false, Width = 24, Height = 24, Visibility = Visibility.Collapsed };
        var status = new InfoBar { IsOpen = false, IsClosable = false };
        content.Children.Add(progress); content.Children.Add(status);
        var storageControls = new StorageSettingsControls(window, row, extensionRow, memoryButton, toolsButton, languagePicker, progress, status);
        var root = new Grid { RequestedTheme = ElementTheme.Light, Background = new SolidColorBrush(Microsoft.UI.Colors.White) };
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        var titleBar = new Grid { Height = 48, Background = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 250, 249, 248)) };
        var title = LocalizedLabel("KYNXA  /  设置");
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
        { storageControls.Message(text, severity); }
        if (extensionLocationError is not null) Message(extensionLocationError.Message, InfoBarSeverity.Error);
        memoryButton.Click += (_, _) =>
        {
            if (_projectActionPending || _sendingPrompt)
                Message(UiText.Get("正在保存聊天，请稍后打开记忆管理。"));
            else OpenMemoryManagement();
        };
        languagePicker.SelectionChanged += (_, _) =>
        {
            if (languagePicker.SelectedItem is not ComboBoxItem { Tag: string selected } || selected == _layout.InterfaceLanguage) return;
            string previous = _layout.InterfaceLanguage;
            _layout.InterfaceLanguage = selected;
            if (!_layoutStateService.Save(_layout))
            {
                _layout.InterfaceLanguage = previous;
                languagePicker.SelectedIndex = previous == "en" ? 1 : 0;
                Message(UiText.Get("界面语言未能保存，请稍后重试。"), InfoBarSeverity.Error);
            }
            else
            {
                if (status.Message == UiText.Get("界面语言未能保存，请稍后重试。")) status.IsOpen = false;
                UiText.Initialize(selected);
            }
        };
        storageControls.SetBusy(false);
        if (StoragePaths.EnvironmentControlled) Message(UiText.Get("数据目录由启动环境变量指定，请移除 KYNXA_DATA_HOME / KYNXA_MODEL_HOME 后再通过设置修改。"), InfoBarSeverity.Warning);
        else if (ExtensionPaths.EnvironmentControlled) Message(UiText.Get("工具与技能目录由启动环境变量指定，请移除 KYNXA_EXTENSION_HOME 后再通过设置修改。"), InfoBarSeverity.Warning);
        void PreventClose(Microsoft.UI.Windowing.AppWindow window, Microsoft.UI.Windowing.AppWindowClosingEventArgs e)
        { if (StoragePaths.IsMigrating) e.Cancel = true; }
        App.Window.AppWindow.Closing += PreventClose;
        window.AppWindow.Closing += PreventClose;
        void CloseSettings(object sender, WindowEventArgs e) => window.Close();
        App.Window.Closed += CloseSettings;
        void UpdateSettingsTitle(object? sender, EventArgs e) => window.Title = UiText.Get("KYNXA · 设置");
        UiText.LanguageChanged += UpdateSettingsTitle;
        window.Closed += (_, _) =>
        {
            App.Window.AppWindow.Closing -= PreventClose;
            App.Window.Closed -= CloseSettings;
            UiText.LanguageChanged -= UpdateSettingsTitle;
            storageControls.Close();
            _storageSettingsWindow = null;
        };
        extensionRow.ChangeButton.Click += async (_, _) => await ChangeExtensionStorageAsync(storageControls);
        choose.Click += async (_, _) =>
        {
            if (StoragePaths.IsMigrating || storageControls.IsBusy) return;
            storageControls.SetBusy(true);
            FileStream? maintenance = null;
            bool acquired = false;
            bool moved = false;
            bool migrationStarted = false;
            try
            {
                if (StoragePaths.EnvironmentControlled) throw new InvalidOperationException(UiText.Get("启动环境变量正在指定数据目录。"));
                if (_memoryManagementWindow is { HasPendingChanges: true })
                    throw new InvalidOperationException(UiText.Get("请先完成记忆编辑，再更改数据存储位置。"));
                _memoryManagementWindow?.CloseForOwner();
                if (_toolManagementWindow is { HasPendingChanges: true })
                    throw new InvalidOperationException(UiText.Get("请先完成工具配置，再更改数据存储位置。"));
                _toolManagementWindow?.CloseForOwner();
                if (_retrievalSettingsWindow is { HasPendingChanges: true })
                    throw new InvalidOperationException(UiText.Get("请先完成检索配置，再更改数据存储位置。"));
                _retrievalSettingsWindow?.CloseForOwner();
                if (_sendingPrompt || _pendingReplies.Values.Any(reply => reply.Error is null)) throw new InvalidOperationException(UiText.Get("请等待模型回复完成后再迁移。"));
                using (var self = System.Diagnostics.Process.GetCurrentProcess())
                {
                    var instances = System.Diagnostics.Process.GetProcessesByName(self.ProcessName);
                    bool another = instances.Any(instance => instance.Id != self.Id);
                    foreach (var instance in instances) instance.Dispose();
                    if (another) throw new InvalidOperationException(UiText.Get("请先关闭其他 KYNXA 实例，再迁移数据。"));
                }
                var address = ModelGatewayService.Address;
                if (address.Scheme != "http" || address.Host is not ("localhost" or "127.0.0.1")) throw new InvalidOperationException(UiText.Get("此设置需要连接本机模型网关。"));
                var picker = new FolderPicker { SuggestedStartLocation = PickerLocationId.ComputerFolder };
                picker.FileTypeFilter.Add("*");
                WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(window));
                var folder = await picker.PickSingleFolderAsync();
                if (folder is null) return;
                if (_storageSettingsWindow != window) return;
                if (_sendingPrompt || _pendingReplies.Values.Any(reply => reply.Error is null)) throw new InvalidOperationException(UiText.Get("请等待模型回复完成后再迁移。"));
                string destination = Path.TrimEndingDirectorySeparator(Path.GetFullPath(folder.Path));
                if (StoragePaths.DataRoot is string currentRoot && string.Equals(destination, Path.TrimEndingDirectorySeparator(currentRoot), StringComparison.OrdinalIgnoreCase))
                { Message(UiText.Get("已经在使用这个目录。"), InfoBarSeverity.Success); return; }
                CaptureProjectDraft(); CaptureStandaloneDraft();
                await _projectStore.SaveAsync(_projects);
                await _projectStore.SaveChatsAsync(_standaloneChats);
                _layoutStateService.Save(_layout);
                StoragePaths.IsMigrating = true;
                migrationStarted = true;
                languagePicker.IsEnabled = false;
                IsEnabled = false;
                progress.IsActive = true; progress.Visibility = Visibility.Visible;
                Message(UiText.Get("正在准备迁移，请保持应用打开…"));
                await ModelGatewayService.EnsureReadyAsync();
                using var http = new HttpClient { BaseAddress = address, Timeout = TimeSpan.FromSeconds(5) };
                var health = await http.GetFromJsonAsync<StorageHealth>("/health");
                if (health?.StorageProtocol != 1 || string.IsNullOrWhiteSpace(health.ModelDataHome)) throw new InvalidOperationException(UiText.Get("当前网关版本不支持存储迁移，请关闭旧网关并重新打开 KYNXA。"));
                string expectedModels = StoragePaths.DataRoot is string dataRoot ? Path.Combine(dataRoot, "Models")
                    : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".kynxa", "models");
                if (!string.Equals(Path.TrimEndingDirectorySeparator(Path.GetFullPath(health.ModelDataHome)),
                    Path.TrimEndingDirectorySeparator(Path.GetFullPath(expectedModels)), StringComparison.OrdinalIgnoreCase))
                    throw new InvalidOperationException(UiText.Get("桌面与网关的数据目录不一致，请统一启动配置后再迁移。"));
                Directory.CreateDirectory(Path.GetDirectoryName(StoragePaths.MigrationLockPath)!);
                maintenance = new FileStream(StoragePaths.MigrationLockPath, FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.Read);
                acquired = true;
                maintenance.SetLength(0);
                await maintenance.WriteAsync(JsonSerializer.SerializeToUtf8Bytes(new { pid = Environment.ProcessId }));
                maintenance.Flush(true);
                health = await http.GetFromJsonAsync<StorageHealth>("/health");
                if (health?.RuntimeCleanupError is not null) throw new InvalidOperationException(UiText.Get("后台工具未安全停止，请重启网关后再迁移。"));
                if (health is null || !health.Migrating || health.ActiveRequests != 0 || !health.MigrationReady)
                    throw new InvalidOperationException(UiText.Get("模型网关还有请求正在处理，请稍后再试。"));
                Message(UiText.Get("正在迁移，请保持应用打开…"));
                string sourceDesktop = StoragePaths.DesktopDirectory, sourceModels = health.ModelDataHome!;
                var updates = new Progress<string>(text => Message(text));
                var result = await Task.Run(() => StorageMigrationService.MoveAsync(sourceDesktop, sourceModels, destination, StoragePaths.PointerPath,
                    updates, initializeTarget: ModelGatewayService.InitializeStorageAsync, migrateExtensions: ExtensionPaths.UsesLegacyRoot));
                StoragePaths.Reload();
                ExtensionPaths.LegacyRoot = result.DataRoot;
                ExtensionPaths.Reload();
                extensionRow.SetPath(ExtensionPaths.Root);
                _projectStore.Dispose();
                _projectStore = new ProjectStore(StoragePaths.DesktopDirectory);
                moved = true;
                _modelSelectionStore = new ModelSelectionStore(StoragePaths.DesktopDirectory);
                foreach (var project in _projects)
                    if (project.FolderPath is string oldPath && StorageMigrationService.IsWithin(oldPath, Path.Combine(sourceDesktop, "Projects")))
                        project.FolderPath = Path.Combine(StoragePaths.DesktopDirectory, Path.GetRelativePath(sourceDesktop, oldPath));
                row.SetPath(result.DataRoot);
                Message(UiText.Get("存储位置已更新，原数据已保留。"), InfoBarSeverity.Success);
            }
            catch (Exception error) { Message(error.Message + UiText.Get(" 若目标目录已生成部分副本，请选择另一个空目录重试。"), InfoBarSeverity.Error); }
            finally
            {
                maintenance?.Dispose();
                if (acquired) { try { File.Delete(StoragePaths.MigrationLockPath); } catch (Exception error) when (error is IOException or UnauthorizedAccessException) { } }
                if (migrationStarted) { StoragePaths.IsMigrating = false; IsEnabled = !moved; }
                if (!moved) storageControls.SetBusy(false);
                progress.IsActive = false; progress.Visibility = Visibility.Collapsed;
            }
            if (moved)
            {
                try
                {
                    // Release maintenance before asking the gateway to load its new root.
                    // 请求网关加载新根目录前，先释放维护状态。
                    Guid? activeWork = _activeProjectChat?.Id, activeChat = _activeStandaloneChat?.Id;
                    var catalog = await _projectStore.LoadAsync();
                    _projects = catalog.Projects;
                    _standaloneChats = catalog.Chats;
                    _activeProjectChat = _projects.SelectMany(project => project.Chats).FirstOrDefault(chat => chat.Id == activeWork);
                    _activeStandaloneChat = _standaloneChats.FirstOrDefault(chat => chat.Id == activeChat);
                    RenderProjects();
                    RebuildStandaloneRows();
                    UpdateConversationPresentation();
                }
                catch (Exception error) { Message(UiText.Get("位置已更新，但会话目录加载失败：") + error.Message, InfoBarSeverity.Error); }
                finally { IsEnabled = true; storageControls.SetBusy(false); }
            }
        };
        window.Activate();
    }
}
