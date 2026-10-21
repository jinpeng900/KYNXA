using System.Net.Http.Json;
using System.Text.Json;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Windowing;
using Windows.Storage;
using Windows.Storage.Pickers;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private Window? _storageSettingsWindow;
    private LocalModelsPage? _localModelsPage;
    private ContentControl? _settingsPageHost;
    private SettingsWorkspace? _settingsWorkspace;
    private string _lastSettingsCategory = "general";
    private sealed record StorageHealth(int StorageProtocol, int ActiveRequests, bool Migrating, string? ModelDataHome,
        bool MigrationReady = false, string? RuntimeCleanupError = null);

    private void OpenLocalModels() => NavigateSettings("local-models");

    private void NavigateSettings(string category)
    {
        if (!_projectsReady || StoragePaths.IsMigrating) return;
        StorageSettings_Click(this, new RoutedEventArgs());
        _settingsWorkspace?.SelectPage(category);
    }

    private FrameworkElement BuildSettingsLocalModelsPage(Window window)
    {
        var page = new LocalModelsPage(window, embedded: true);
        _localModelsPage = page;
        void RefreshChoices(object? sender, EventArgs args)
        {
            if (!_chatClosing && !_projectViewClosed && !_closeApproved) _ = RefreshModelPickerAsync();
        }
        page.ModelsChanged += RefreshChoices;
        page.BackRequested += (_, _) => _settingsWorkspace?.SelectPage("general");
        window.Closed += (_, _) => page.ModelsChanged -= RefreshChoices;
        return page;
    }

    private void StorageSettings_Click(object sender, RoutedEventArgs args)
    {
        if (!_projectsReady) return;
        if (_storageSettingsWindow is { } existing)
        {
            // Window.Activate already restores an iconic HWND; use one native presentation path for reuse and restore.
            // Window.Activate 自带最小化恢复；复用与恢复只走一次原生展示流程。
            existing.Activate();
            return;
        }
        Exception? extensionLocationError = null;
        try { ExtensionPaths.Reload(); }
        catch (Exception error) { extensionLocationError = error; }
        var window = new Window { Title = UiText.Get("KYNXA · 设置") };
        _storageSettingsWindow = window;
        var font = (FontFamily)Application.Current.Resources["KynxaUIFont"];
        TextBlock Label(string text, double size = 14) => new() { Text = text, FontSize = size, FontFamily = font,
            Foreground = AppearanceService.GetBrush("KynxaTextBrush"), TextWrapping = TextWrapping.Wrap };
        TextBlock LocalizedLabel(string key, double size = 14)
        {
            var text = Label(string.Empty, size);
            UiLocalization.Bind(text, TextBlock.TextProperty, key);
            return text;
        }
        var languageRow = new Grid { ColumnSpacing = 20, Padding = new Thickness(14, 10, 14, 10), CornerRadius = new CornerRadius(12),
            Background = AppearanceService.GetBrush("KynxaSettingsCardBrush") };
        languageRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        languageRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var languageLabel = LocalizedLabel("界面语言");
        languageLabel.VerticalAlignment = VerticalAlignment.Center;
        var languagePicker = new ScrollSafeComboBox { FontFamily = font, FontSize = 13,
            CornerRadius = new CornerRadius(9), MinWidth = 140, VerticalAlignment = VerticalAlignment.Center };
        var focusBrush = AppearanceService.GetBrush("KynxaSecondaryTextBrush");
        var selectedBrush = AppearanceService.GetBrush("KynxaSelectionBrush");
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
        var appearanceSection = new AppearanceSettingsSection(font, preferences =>
        {
            TrySetAppearancePreferences(preferences, out string? errorKey);
            return errorKey;
        }, () => new AppearanceTransferPreferences(_layout.AppearancePaletteId, _layout.AppearanceMode,
            _layout.ChatFontSize, _layout.AppearanceAccent), isImport => PickAppearanceFileAsync(window, isImport), hideHeading: true);
        var row = new StorageLocationRow("数据存储", StoragePaths.DataRoot ?? StoragePaths.DesktopDirectory,
            "StorageDirectoryPath", "StorageDirectoryChooseButton");
        var choose = row.ChangeButton;
        var extensionRow = new StorageLocationRow("用户工具", ExtensionPaths.Root,
            "ExtensionStorageDirectoryPath", "ExtensionStorageDirectoryChooseButton");
        var progress = new ProgressRing { IsActive = false, Width = 24, Height = 24, Visibility = Visibility.Collapsed };
        var status = new InfoBar { IsOpen = false, IsClosable = false };
        AutomationProperties.SetLiveSetting(status, Microsoft.UI.Xaml.Automation.Peers.AutomationLiveSetting.Polite);
        var storageControls = new StorageSettingsControls(window, row, extensionRow, languagePicker, progress, status);
        var root = new Grid { Background = AppearanceService.GetBrush("KynxaMainBrush") };
        AutomationProperties.SetAutomationId(root, "AppearanceSettingsRoot");
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        var titleBar = new Grid { Height = 48, Background = AppearanceService.GetBrush("KynxaTitleBarBrush") };
        var title = LocalizedLabel("KYNXA  /  设置");
        title.Margin = new Thickness(20, 0, 140, 0);
        title.VerticalAlignment = VerticalAlignment.Center;
        titleBar.Children.Add(title);
        // Categories retain their views, including drafts, downloads and scroll positions.
        // 分类保留原页面实例，包括草稿、下载与滚动位置，不再打开其他设置窗口。
        var workspace = new SettingsWorkspace(font);
        _settingsWorkspace = workspace;
        var generalPage = new StackPanel { Spacing = 14 };
        generalPage.Children.Add(languageRow);
        var storagePage = new StackPanel { Spacing = 14 };
        storagePage.Children.Add(row);
        storagePage.Children.Add(extensionRow);
        workspace.AddPage("general", "通用", "\uE713", () => SettingsWorkspace.ScrollPage(generalPage), font);
        workspace.AddPage("appearance", "外观", "\uE790", () => SettingsWorkspace.ScrollPage(appearanceSection.Root), font,
            headerAction: appearanceSection.HeaderAction);
        workspace.AddPage("memory", "记忆", "\uE8F1", () => BuildSettingsMemoryPage(window), font);
        workspace.AddPage("local-models", "本地模型", "\uE950", () => BuildSettingsLocalModelsPage(window), font);
        workspace.AddPage("storage", "存储", "\uE8B7", () => SettingsWorkspace.ScrollPage(storagePage), font);
        workspace.AddPage("tools", "工具与技能", "\uE912", () => BuildSettingsToolsPage(window), font);
        workspace.AddPage("retrieval", "检索与网页搜索", "\uE721", () => BuildSettingsRetrievalPage(window), font);
        workspace.AddPage("about", "关于", "\uE946", () =>
        {
            var about = new StackPanel { Spacing = 12 };
            about.Children.Add(new TextBlock { Text = "KYNXA", FontFamily = font, FontSize = 22,
                Foreground = AppearanceService.GetBrush("KynxaTextBrush") });
            about.Children.Add(new TextBlock { Text = typeof(ShellPage).Assembly.GetName().Version?.ToString(2) ?? "1.65",
                FontFamily = font, FontSize = 14, Foreground = AppearanceService.GetBrush("KynxaSecondaryTextBrush") });
            return SettingsWorkspace.ScrollPage(about);
        }, font);
        void RefreshSettingsContext(string category)
        {
            bool current = category switch
            {
                "memory" => RefreshSettingsMemoryContext(),
                "tools" => RefreshSettingsToolsContext(),
                _ => true
            };
            const string pendingScope = "当前页面仍对应原范围，请先保存或放弃编辑，再切换范围。";
            if (!current) storageControls.Message(pendingScope, InfoBarSeverity.Warning);
            else if (status.Message == UiText.Get(pendingScope)) status.IsOpen = false;
        }
        workspace.PageChanged += (_, category) =>
        {
            _lastSettingsCategory = category;
            RefreshSettingsContext(category);
        };
        window.Activated += (_, args) =>
        {
            if (args.WindowActivationState != WindowActivationState.Deactivated && workspace.SelectedKey is { } category)
                RefreshSettingsContext(category);
        };
        storageControls.Navigation = workspace;
        var pageHost = new ContentControl { Content = workspace, HorizontalContentAlignment = HorizontalAlignment.Stretch,
            VerticalContentAlignment = VerticalAlignment.Stretch };
        _settingsPageHost = pageHost;
        Grid.SetRow(pageHost, 1);
        root.Children.Add(titleBar); root.Children.Add(pageHost);
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        var feedback = new StackPanel { Spacing = 6, Margin = new Thickness(16, 0, 16, 10) };
        feedback.Children.Add(progress);
        feedback.Children.Add(status);
        Grid.SetRow(feedback, 2);
        root.Children.Add(feedback);
        workspace.SelectPage(_lastSettingsCategory);
        window.Content = root;
        window.ExtendsContentIntoTitleBar = true;
        window.SetTitleBar(titleBar);
        AppearanceService.TrackWindow(window);
        var ownerBounds = App.Window.AppWindow;
        // Resolve DPI from the visible owner and apply final bounds before the first activation.
        // 使用已显示所属窗口的 DPI，在首次激活前设置最终尺寸，避免 Loaded 后再次缩放引起跳变。
        double scale = XamlRoot?.RasterizationScale ?? 1;
        if (window.AppWindow.Presenter is OverlappedPresenter settingsPresenter)
        {
            settingsPresenter.PreferredMinimumWidth = (int)Math.Ceiling(700 * scale);
            settingsPresenter.PreferredMinimumHeight = (int)Math.Ceiling(460 * scale);
        }
        var area = DisplayArea.GetFromWindowId(ownerBounds.Id, DisplayAreaFallback.Primary).WorkArea;
        int width = Math.Min((int)Math.Ceiling(1000 * scale), Math.Max(1, area.Width - 32));
        int height = Math.Min((int)Math.Ceiling(720 * scale), Math.Max(1, area.Height - 32));
        int x = Math.Clamp(ownerBounds.Position.X + (ownerBounds.Size.Width - width) / 2, area.X, area.X + area.Width - width);
        int y = Math.Clamp(ownerBounds.Position.Y + (ownerBounds.Size.Height - height) / 2, area.Y, area.Y + area.Height - height);
        window.AppWindow.MoveAndResize(new Windows.Graphics.RectInt32(x, y, width, height));
        void Message(string text, InfoBarSeverity severity = InfoBarSeverity.Informational)
        { storageControls.Message(text, severity); }
        if (extensionLocationError is not null) Message(extensionLocationError.Message, InfoBarSeverity.Error);
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
        bool ownerClosing = false;
        App.Window.AppWindow.Closing += PreventClose;
        window.AppWindow.Closing += (sender, e) =>
        {
            PreventClose(sender, e);
            if (ownerClosing || e.Cancel) return;
            string? pending = _memoryManagementWindow is { HasPendingChanges: true } ? "memory"
                : _toolManagementWindow is { HasPendingChanges: true } ? "tools"
                : _retrievalSettingsWindow is { HasPendingChanges: true } ? "retrieval" : null;
            if (pending is null) return;
            e.Cancel = true;
            workspace.SelectPage(pending);
            Message(UiText.Get("请先保存或放弃当前编辑，再关闭设置。"), InfoBarSeverity.Warning);
        };
        void CloseSettings(object sender, WindowEventArgs e) { ownerClosing = true; window.Close(); }
        App.Window.Closed += CloseSettings;
        void UpdateSettingsTitle(object? sender, EventArgs e) => window.Title = UiText.Get("KYNXA · 设置");
        UiText.LanguageChanged += UpdateSettingsTitle;
        window.Closed += (_, _) =>
        {
            App.Window.AppWindow.Closing -= PreventClose;
            App.Window.Closed -= CloseSettings;
            UiText.LanguageChanged -= UpdateSettingsTitle;
            appearanceSection.Dispose();
            _memoryManagementWindow?.DisposeEmbedded();
            _memoryManagementWindow = null;
            _settingsMemoryTargets = null;
            _toolManagementWindow?.DisposeEmbedded();
            _toolManagementWindow = null;
            _settingsToolsConversationId = null;
            _retrievalSettingsWindow?.DisposeEmbedded();
            _retrievalSettingsWindow = null;
            storageControls.Close();
            _localModelsPage?.Dispose();
            _localModelsPage = null;
            _settingsPageHost = null;
            workspace.Dispose();
            _settingsWorkspace = null;
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
                _memoryManagementWindow?.DisposeEmbedded();
                _memoryManagementWindow = null;
                workspace.ResetPage("memory");
                if (_toolManagementWindow is { HasPendingChanges: true })
                    throw new InvalidOperationException(UiText.Get("请先完成工具配置，再更改数据存储位置。"));
                _toolManagementWindow?.DisposeEmbedded();
                _toolManagementWindow = null;
                workspace.ResetPage("tools");
                if (_retrievalSettingsWindow is { HasPendingChanges: true })
                    throw new InvalidOperationException(UiText.Get("请先完成检索配置，再更改数据存储位置。"));
                _retrievalSettingsWindow?.DisposeEmbedded();
                _retrievalSettingsWindow = null;
                workspace.ResetPage("retrieval");
                if (_sendingPrompt || _pendingReplies.Count > 0 || _retryingReplies.Count > 0) throw new InvalidOperationException(UiText.Get("请等待模型回复完成后再迁移。"));
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
                if (_sendingPrompt || _pendingReplies.Count > 0 || _retryingReplies.Count > 0) throw new InvalidOperationException(UiText.Get("请等待模型回复完成后再迁移。"));
                string destination = Path.TrimEndingDirectorySeparator(Path.GetFullPath(folder.Path));
                if (StoragePaths.DataRoot is string currentRoot && string.Equals(destination, Path.TrimEndingDirectorySeparator(currentRoot), StringComparison.OrdinalIgnoreCase))
                { Message(UiText.Get("已经在使用这个目录。"), InfoBarSeverity.Success); return; }
                CaptureProjectDraft(); CaptureStandaloneDraft();
                await _projectStore.SaveAsync(_projects);
                await _projectStore.SaveChatsAsync(_standaloneChats);
                _layoutStateService.Save(_layout);
                await MessageTimePresentation.FlushAsync();
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
                string expectedModels = ApplicationPaths.ModelHome;
                if (!string.Equals(Path.TrimEndingDirectorySeparator(Path.GetFullPath(health.ModelDataHome)),
                    Path.TrimEndingDirectorySeparator(Path.GetFullPath(expectedModels)), StringComparison.OrdinalIgnoreCase))
                    throw new InvalidOperationException(UiText.Get("桌面与网关的数据目录不一致，请统一启动配置后再迁移。"));
                Directory.CreateDirectory(Path.GetDirectoryName(StoragePaths.MigrationLockPath)!);
                maintenance = new FileStream(StoragePaths.MigrationLockPath, FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.Read);
                acquired = true;
                maintenance.SetLength(0);
                await maintenance.WriteAsync(JsonSerializer.SerializeToUtf8Bytes(new { pid = Environment.ProcessId }));
                maintenance.Flush(true);
                // Maintenance settles background writers asynchronously; wait without cancelling user work.
                // 维护状态异步结算后台写入；有界等待，不强制取消用户任务。
                var maintenanceDeadline = DateTime.UtcNow.AddSeconds(30);
                do
                {
                    health = await http.GetFromJsonAsync<StorageHealth>("/health", storageControls.Token);
                    if (health?.RuntimeCleanupError is not null) throw new InvalidOperationException(UiText.Get("后台工具未安全停止，请重启网关后再迁移。"));
                    if (health?.StorageProtocol != 1 || string.IsNullOrWhiteSpace(health.ModelDataHome) ||
                        !string.Equals(Path.TrimEndingDirectorySeparator(Path.GetFullPath(health.ModelDataHome)),
                            Path.TrimEndingDirectorySeparator(Path.GetFullPath(expectedModels)), StringComparison.OrdinalIgnoreCase))
                        throw new InvalidOperationException(UiText.Get("桌面与网关的数据目录不一致，请统一启动配置后再迁移。"));
                    if (health.Migrating && health.ActiveRequests == 0 && health.MigrationReady) break;
                    if (DateTime.UtcNow >= maintenanceDeadline)
                        throw new InvalidOperationException(UiText.Get("模型网关还有请求正在处理，请稍后再试。"));
                    await Task.Delay(250, storageControls.Token);
                } while (true);
                Message(UiText.Get("正在迁移，请保持应用打开…"));
                string sourceDesktop = StoragePaths.DesktopDirectory, sourceModels = health.ModelDataHome!;
                var updates = new Progress<string>(text => Message(text));
                var result = await Task.Run(() => StorageMigrationService.MoveAsync(sourceDesktop, sourceModels, destination, StoragePaths.PointerPath,
                    updates, initializeTarget: ModelGatewayService.InitializeStorageAsync, migrateExtensions: ExtensionPaths.UsesLegacyRoot));
                StoragePaths.Reload();
                MessageTimePresentation.ConfigureCache(StoragePaths.DesktopDirectory);
                if (ExtensionPaths.UsesLegacyRoot) ExtensionPaths.LegacyRoot = result.DataRoot;
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
