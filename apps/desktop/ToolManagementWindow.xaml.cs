using System.Collections.ObjectModel;
using System.Net;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Storage.Pickers;

namespace KYNXA_Desktop;

/// <summary>
/// Edits gateway configuration and displays skills as text; reading never starts an MCP process.
/// 编辑网关配置并以文本展示技能；读取配置不会启动 MCP 进程。
/// </summary>
public sealed partial class ToolManagementWindow : Window
{
    private readonly IAgentApi _api;
    private readonly bool _ownsApi;
    private readonly Guid? _conversationId;
    private readonly CancellationTokenSource _lifetime = new();
    private readonly ObservableCollection<McpServerConfig> _servers = [];
    private readonly ObservableCollection<string> _directories = [];
    private AgentConfig? _config;
    private CancellationTokenSource? _preview;
    private int _previewGeneration;
    private int _activeTabIndex;
    private int _serverSourceIndex;
    private string? _editingServerId;
    private bool _busy, _updating, _closed, _allowClose, _serverDirty, _directoriesDirty, _dialogOpen;
    private string? _noticeKey;
    private string _noticeDetails = string.Empty;
    private ServerEditorState? _savedEditor;
    private AgentTool? _editingTool;
    private bool _toolDirty;
    private AgentSkill? _editingSkill;
    private AgentSkill[] _skills = [];
    private bool _skillDirty;
    private McpCatalogResponse _catalog = new([], []);
    private McpConnectionDiagnostic[] _connections = [];
    private sealed record ServerEditorState(string Id, string Name, string Command, string Arguments, bool Enabled,
        string Transport, string Cwd, string EnvRefs, string Url, string HeaderEnv, string Auth,
        int BrowserMode, bool BrowserVisible, string BrowserEndpoint);

    public ToolManagementWindow(IAgentApi? api = null, Guid? conversationId = null)
    {
        InitializeComponent();
        _api = api ?? new AgentApiClient();
        _ownsApi = api is null;
        _conversationId = conversationId;
        AgentServerList.ItemsSource = _servers;
        AgentDirectoryList.ItemsSource = _directories;
        UiLocalization.Bind(AgentServersTab, PivotItem.HeaderProperty, "MCP 服务");
        UiLocalization.Bind(AgentSkillsTab, PivotItem.HeaderProperty, "技能");
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AgentTitleBar);
        AppWindow.Resize(new Windows.Graphics.SizeInt32(940, 720));
        if (AppWindowTitleBar.IsCustomizationSupported())
        {
            AppWindow.TitleBar.ButtonBackgroundColor = Microsoft.UI.Colors.Transparent;
            AppWindow.TitleBar.ButtonInactiveBackgroundColor = Microsoft.UI.Colors.Transparent;
            AppWindow.TitleBar.ButtonForegroundColor = Microsoft.UI.Colors.Black;
        }
        UiText.LanguageChanged += Language_Changed;
        AppWindow.Closing += Window_Closing;
        Closed += Window_Closed;
        Language_Changed(null, EventArgs.Empty);
        FillServer(null);
        SetBusy(false);
    }

    public bool HasPendingChanges => _busy || _serverDirty || _directoriesDirty || _toolDirty || _skillDirty;

    public void ShowSection(bool skills)
    {
        if (_closed || _dialogOpen) return;
        // Both editors stay alive; switching the visible section does not discard any draft.
        // 两个编辑器始终保留；只切换可见页签，不丢弃任何草稿。
        AgentTabs.SelectedIndex = skills ? 1 : 0;
    }

    public void CloseForOwner()
    {
        _allowClose = true;
        Close();
    }

    private async void AgentRoot_Loaded(object sender, RoutedEventArgs e)
    {
        ApplyAdaptiveLayout(AgentRoot.ActualWidth);
        await LoadAsync();
    }

    private void AgentRoot_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (_api is null || _closed) return;
        ApplyAdaptiveLayout(e.NewSize.Width);
    }

    private void ApplyAdaptiveLayout(double width)
    {
        bool narrow = width > 0 && width < 780;
        var full = new GridLength(1, GridUnitType.Star);
        double listHeight = narrow ? Math.Clamp(AgentRoot.ActualHeight * 0.22, 92, 180) : double.PositiveInfinity;
        // Keep long forms reachable in short stacked windows; the controls retain their state when reparented.
        // 短窗采用上下布局时，将额外操作并入可滚动表单；迁移控件父级不重建草稿或展开状态。
        if (narrow && !AgentServerEditorBody.Children.Contains(AgentConnectionPanel))
        {
            AgentServersGrid.Children.Remove(AgentConnectionPanel);
            AgentServerEditorBody.Children.Insert(0, AgentConnectionPanel);
        }
        else if (!narrow && AgentServerEditorBody.Children.Contains(AgentConnectionPanel))
        {
            AgentServerEditorBody.Children.Remove(AgentConnectionPanel);
            AgentServersGrid.Children.Add(AgentConnectionPanel);
        }
        if (narrow && !AgentSkillEditorBody.Children.Contains(AgentSkillDirectoriesExpander))
        {
            AgentSkillsGrid.Children.Remove(AgentSkillDirectoriesExpander);
            AgentSkillEditorBody.Children.Insert(0, AgentSkillDirectoriesExpander);
        }
        else if (!narrow && AgentSkillEditorBody.Children.Contains(AgentSkillDirectoriesExpander))
        {
            AgentSkillEditorBody.Children.Remove(AgentSkillDirectoriesExpander);
            AgentSkillsGrid.Children.Add(AgentSkillDirectoriesExpander);
        }
        AgentContentGrid.Margin = narrow ? new Thickness(16, 14, 16, 16) : new Thickness(24, 14, 24, 24);
        AgentServersGrid.ColumnDefinitions[0].Width = narrow ? full : new GridLength(240);
        AgentServersGrid.ColumnDefinitions[1].Width = narrow ? new GridLength(0) : full;
        AgentServersGrid.ColumnSpacing = narrow ? 0 : 20;
        AgentServersGrid.RowDefinitions[1].Height = narrow ? GridLength.Auto : full;
        AgentServersGrid.RowDefinitions[2].Height = narrow ? full : GridLength.Auto;
        AgentServersGrid.RowDefinitions[3].Height = new GridLength(0);
        AgentServerListPanel.MaxHeight = listHeight;
        Grid.SetColumn(AgentServerEditor, narrow ? 0 : 1);
        Grid.SetRow(AgentServerEditor, narrow ? 2 : 1);
        Grid.SetColumn(AgentConnectionPanel, narrow ? 0 : 1);
        Grid.SetRow(AgentConnectionPanel, narrow ? 3 : 2);

        AgentSkillsGrid.ColumnDefinitions[0].Width = narrow ? full : new GridLength(240);
        AgentSkillsGrid.ColumnDefinitions[1].Width = narrow ? new GridLength(0) : full;
        AgentSkillsGrid.ColumnSpacing = narrow ? 0 : 20;
        AgentSkillsGrid.RowDefinitions[2].Height = narrow ? GridLength.Auto : full;
        AgentSkillsGrid.RowDefinitions[3].Height = narrow ? full : new GridLength(0);
        AgentSkillListPanel.MaxHeight = listHeight;
        Grid.SetColumn(AgentSkillEditor, narrow ? 0 : 1);
        Grid.SetRow(AgentSkillEditor, narrow ? 3 : 2);

        AgentServerToolbar.ColumnDefinitions[0].Width = narrow ? full : new GridLength(140);
        AgentServerToolbar.ColumnDefinitions[1].Width = full;
        AgentServerToolbar.ColumnDefinitions[2].Width = narrow ? new GridLength(0) : GridLength.Auto;
        AgentServerToolbar.ColumnDefinitions[3].Width = narrow ? new GridLength(0) : GridLength.Auto;
        AgentServerToolbar.RowDefinitions[1].Height = narrow ? GridLength.Auto : new GridLength(0);
        Grid.SetRow(AgentAddPresetButton, narrow ? 1 : 0);
        Grid.SetColumn(AgentAddPresetButton, narrow ? 0 : 2);
        Grid.SetRow(AgentNewServerButton, narrow ? 1 : 0);
        Grid.SetColumn(AgentNewServerButton, narrow ? 1 : 3);
        var orientation = width > 0 && width < 500 ? Orientation.Vertical : Orientation.Horizontal;
        AgentServerActions.Orientation = AgentConnectionActions.Orientation = AgentSkillActions.Orientation = orientation;
        AgentDirectoryActions.Orientation = AgentToolActions.Orientation = orientation;
    }

    private void Language_Changed(object? sender, EventArgs e)
    {
        if (_closed) return;
        if (!DispatcherQueue.HasThreadAccess) { DispatcherQueue.TryEnqueue(() => Language_Changed(sender, e)); return; }
        Title = UiText.Get("KYNXA · 工具与技能");
        if (_noticeKey is not null) AgentStatusBar.Message = UiText.Get(_noticeKey) + _noticeDetails;
        RefreshConnectionStatus();
        RenderServerSource();
        if (_config is { } config) RenderToolLocations(config);
        RenderSkillDiagnostics(); Preset_SelectionChanged(this, null!);
        if (_editingSkill is { } skill) UiLocalization.Bind(AgentSkillSourceTypeLabel, TextBlock.TextProperty, SkillSourceKey(skill));
        int transportIndex = AgentServerTransportBox.SelectedIndex;
        int browserModeIndex = AgentBrowserModeBox.SelectedIndex;
        bool wasUpdating = _updating; _updating = true;
        AgentServerTransportBox.SelectedIndex = -1;
        AgentServerTransportBox.SelectedIndex = transportIndex;
        AgentServerSourceBox.SelectedIndex = -1;
        AgentServerSourceBox.SelectedIndex = _serverSourceIndex;
        AgentBrowserModeBox.SelectedIndex = -1;
        AgentBrowserModeBox.SelectedIndex = browserModeIndex;
        _updating = wasUpdating;
        RefreshBrowserFields();
    }

    private void Notice(string key, InfoBarSeverity severity = InfoBarSeverity.Warning, Exception? error = null)
    {
        _noticeKey = key;
        _noticeDetails = error is GatewayApiException ? " " + error.Message : string.Empty;
        AgentStatusBar.Message = UiText.Get(key) + _noticeDetails;
        AgentStatusBar.Severity = severity;
        AgentStatusBar.IsOpen = true;
    }

    private void SetBusy(bool value)
    {
        _busy = value;
        AgentLoadingRing.IsActive = value;
        AgentLoadingRing.Visibility = value ? Visibility.Visible : Visibility.Collapsed;
        AgentRefreshButton.IsEnabled = AgentConnectButton.IsEnabled = !value;
        AgentServerEditor.IsEnabled = AgentServerList.IsEnabled = AgentNewServerButton.IsEnabled = !value && _config is not null;
        AgentServerSearchBox.IsEnabled = AgentSkillSearchBox.IsEnabled = !value;
        AgentAddDirectoryButton.IsEnabled = !value && _config is not null;
        AgentRemoveDirectoryButton.IsEnabled = !value && _config is not null && AgentDirectoryList.SelectedItem is string;
        AgentSaveDirectoriesButton.IsEnabled = !value && _config is not null && _directoriesDirty;
        AgentImportSkillButton.IsEnabled = !value && _config is not null;
        var editingServer = _config?.McpServers.FirstOrDefault(server => server.Id == _editingServerId);
        bool official = IsOfficial(editingServer);
        AgentDeleteServerButton.Visibility = official ? Visibility.Collapsed : Visibility.Visible;
        AgentDeleteServerButton.IsEnabled = !value && editingServer is not null && !official;
        bool canRestore = official && editingServer?.Overridden == true && OfficialPresetFor(editingServer) is not null;
        AgentRestoreServerButton.Visibility = canRestore ? Visibility.Visible : Visibility.Collapsed;
        AgentRestoreServerButton.IsEnabled = !value && canRestore;
        RefreshServerSaveAvailability();
        AgentToolList.IsEnabled = !value;
        AgentToolEnabledBox.IsEnabled = !value && SelectedToolServer() is not null;
        AgentSaveToolButton.IsEnabled = !value && _toolDirty && SelectedToolServer() is not null;
        AgentPresetBox.IsEnabled = !value && _config is not null;
        AgentServerSourceBox.IsEnabled = !value && _config is not null;
        AgentAddPresetButton.IsEnabled = !value && _config is not null && AgentPresetBox.SelectedItem is McpPreset;
        AgentReconnectButton.IsEnabled = !value && _editingServerId is not null && _config?.McpServers.Any(server => server.Id == _editingServerId && server.Enabled) == true;
        AgentDisconnectButton.IsEnabled = !value && _editingServerId is not null;
        AgentSkillEnabledBox.IsEnabled = !value && _config is not null && _editingSkill is not null;
        AgentSaveSkillButton.IsEnabled = !value && _config is not null && _skillDirty && _editingSkill is not null;
        AgentCancelServerButton.IsEnabled = AgentCancelSkillButton.IsEnabled = !value;
        AgentCancelServerButton.Visibility = _serverDirty ? Visibility.Visible : Visibility.Collapsed;
        AgentCancelSkillButton.Visibility = _skillDirty ? Visibility.Visible : Visibility.Collapsed;
        AgentClearServerFiltersButton.IsEnabled = AgentClearSkillSearchButton.IsEnabled = !value;
        AgentPendingLabel.Visibility = value || _serverDirty || _directoriesDirty || _toolDirty || _skillDirty
            ? Visibility.Visible : Visibility.Collapsed;
        UiLocalization.Bind(AgentPendingLabel, TextBlock.TextProperty, value ? "正在处理，请稍候。" : "还有未保存的修改");
    }

    private void RefreshServerSaveAvailability()
    {
        AgentSaveServerButton.IsEnabled = !_busy && _config is not null && (_editingServerId is null || _serverDirty);
    }

    private void AcceptConfig(AgentConfig config, bool replaceDirectories)
    {
        if (config.Version != 1 || config.Revision < 0 || config.McpServers is null || config.SkillDirectories is null)
            throw new InvalidDataException(UiText.Get("工具配置版本不受支持。"));
        _config = config;
        RefreshServerList();
        _updating = true;
        if (replaceDirectories)
        {
            _directories.Clear();
            foreach (string directory in config.SkillDirectories) _directories.Add(directory);
            _directoriesDirty = false;
        }
        _updating = false;
        RenderToolLocations(config);
        RenderServerSource();
    }

    private async Task LoadAsync()
    {
        if (_busy || _closed) return;
        SetBusy(true);
        try
        {
            var configTask = _api.GetConfigAsync(_lifetime.Token);
            var skillsTask = _api.GetSkillsAsync(_conversationId, _lifetime.Token);
            var toolsTask = _api.GetToolsAsync(_lifetime.Token);
            var catalogTask = _api.GetMcpCatalogAsync(_lifetime.Token);
            await Task.WhenAll(configTask, skillsTask, toolsTask, catalogTask);
            if (_closed) return;
            AcceptConfig(await configTask, replaceDirectories: true);
            AcceptSkills(await skillsTask, discardDraft: true);
            _catalog = await catalogTask;
            AgentPresetBox.ItemsSource = _catalog.Presets;
            var tools = await toolsTask;
            AcceptTools(tools.Tools);
            AcceptConnections(tools.Connections);
            FillServer(_config!.McpServers.FirstOrDefault(server => server.Id == _editingServerId));
            AgentStatusBar.IsOpen = false;
            _noticeKey = null;
            if (tools.Errors is { Length: > 0 }) ConnectionErrors(tools.Errors);
        }
        catch (OperationCanceledException) when (_lifetime.IsCancellationRequested) { }
        catch (Exception error) { if (!_closed) Notice("工具列表读取失败，请重试。", InfoBarSeverity.Error, error); }
        finally { if (!_closed) SetBusy(false); }
    }

    private async Task<bool> SaveAsync(McpServerConfig[] servers, bool saveDirectories, string[]? disabledSkills = null)
    {
        if (_config is null || _busy || _closed) return false;
        SetBusy(true);
        try
        {
            string[] directories = saveDirectories ? _directories.ToArray() : _config.SkillDirectories;
            var saved = await _api.SaveConfigAsync(new(1, _config.Revision, servers, directories,
                disabledSkills ?? _config.DisabledSkills ?? [], _config.DisabledOfficialMcpServers), _lifetime.Token);
            if (_closed) return false;
            AcceptConfig(saved, replaceDirectories: saveDirectories);
            Notice("工具配置已保存。", InfoBarSeverity.Success);
            return true;
        }
        catch (GatewayApiException error) when (error.StatusCode == HttpStatusCode.Conflict)
        {
            try
            {
                var latest = await _api.GetConfigAsync(_lifetime.Token);
                if (!_closed) AcceptConfig(latest, replaceDirectories: false);
                if (!_closed) Notice("配置已更新。编辑已保留，请核对列表后再次保存。", error: error);
            }
            catch (Exception) { if (!_closed) { _config = null; Notice("配置冲突且刷新失败，请先刷新列表。", InfoBarSeverity.Error); } }
            return false;
        }
        catch (OperationCanceledException) when (_lifetime.IsCancellationRequested) { return false; }
        catch (Exception error) { if (!_closed) Notice("工具配置保存失败。", InfoBarSeverity.Error, error); return false; }
        finally { if (!_closed) SetBusy(false); }
    }

    private void FillServer(McpServerConfig? server)
    {
        _updating = true;
        _editingServerId = server?.Id;
        AgentServerIdBox.Text = server?.Id ?? "mcp-" + Guid.NewGuid().ToString("N")[..8];
        AgentServerIdBox.IsReadOnly = server is not null;
        AgentServerNameBox.Text = server?.Name ?? string.Empty;
        AgentServerCommandBox.Text = server?.Command ?? string.Empty;
        AgentServerArgsBox.Text = JsonSerializer.Serialize(server?.Args ?? []);
        AgentServerEnabledBox.IsChecked = server?.Enabled ?? false;
        AgentServerTransportBox.SelectedIndex = server?.Transport == "streamable-http" ? 1 : 0;
        AgentServerCwdBox.Text = server?.Cwd ?? "";
        AgentServerEnvRefsBox.Text = JsonSerializer.Serialize(server?.EnvRefs ?? []);
        AgentServerUrlBox.Text = server?.Url ?? "";
        AgentServerHeaderEnvBox.Text = JsonSerializer.Serialize(server?.HeaderEnv ?? []);
        AgentServerAuthBox.Text = server?.Auth is { } auth ? JsonSerializer.Serialize(auth,
            new JsonSerializerOptions(JsonSerializerDefaults.Web) { DefaultIgnoreCondition = System.Text.Json.Serialization.JsonIgnoreCondition.WhenWritingNull }) : "";
        RefreshTransportFields();
        FillBrowserSettings();
        _savedEditor = ReadEditor();
        _serverDirty = false;
        _updating = false;
        RenderServerSource();
        RefreshFilteredHints();
        RefreshConnectionStatus(); SetBusy(_busy);
    }

    private static bool IsOfficial(McpServerConfig? server) => server?.Origin == "official";

    private static string ServerSourceKey(McpServerConfig? server) => IsOfficial(server)
        ? server!.Overridden == true ? "官方 · 已自定义" : "官方工具"
        : "用户工具";

    private static string SkillSourceKey(AgentSkill skill) => skill.Origin switch
    {
        "builtin" => "官方工具",
        "workspace" => "项目工具",
        _ => "用户工具"
    };

    private McpPreset? OfficialPresetFor(McpServerConfig? server) => IsOfficial(server) && server?.PresetId is { } id
        ? _catalog.Presets.FirstOrDefault(preset => preset.Id == id) : null;

    private void RenderServerSource()
    {
        var server = _config?.McpServers.FirstOrDefault(item => item.Id == _editingServerId);
        AgentServerSourceLabel.Text = UiText.Get(ServerSourceKey(server)) + (IsOfficial(server)
            ? " · " + UiText.Get("修改保存在用户工具中。") : string.Empty);
    }

    private void RenderToolLocations(AgentConfig config)
    {
        void Fill(TextBlock label, TextBlock path, string? location, string? version = null)
        {
            bool visible = !string.IsNullOrWhiteSpace(location);
            label.Visibility = path.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
            path.Text = location ?? string.Empty;
            ToolTipService.SetToolTip(path, location + (version is null ? string.Empty : "\n" + string.Format(UiText.Get("版本 {0}"), version)));
        }
        Fill(AgentOfficialRootLabel, AgentOfficialRootPath, config.OfficialToolsRoot, config.OfficialPackageVersion);
        Fill(AgentUserRootLabel, AgentUserRootPath, config.UserToolsRoot);
    }

    private void RefreshServerList()
    {
        bool wasUpdating = _updating;
        _updating = true;
        string query = AgentServerSearchBox.Text.Trim();
        _servers.Clear();
        foreach (var server in _config?.McpServers ?? [])
            if ((_serverSourceIndex == 0 || (_serverSourceIndex == 1) == IsOfficial(server)) &&
                (query.Length == 0 || server.Name.Contains(query, StringComparison.OrdinalIgnoreCase) ||
                    server.Id.Contains(query, StringComparison.OrdinalIgnoreCase))) _servers.Add(server);
        AgentServerList.SelectedItem = _servers.FirstOrDefault(server => server.Id == _editingServerId);
        _updating = wasUpdating;
        UiLocalization.Bind(AgentServerEmptyLabel, TextBlock.TextProperty,
            (_config?.McpServers.Length ?? 0) == 0 ? "尚无 MCP 服务，请添加服务或预设。" : "没有匹配的服务，请换个关键词或清除筛选。");
        AgentServerEmptyLabel.Visibility = _servers.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        AgentServerEmptyPanel.Visibility = AgentServerEmptyLabel.Visibility;
        AgentClearServerFiltersButton.Visibility = _servers.Count == 0 && (_config?.McpServers.Length ?? 0) > 0 &&
            (query.Length > 0 || _serverSourceIndex != 0) ? Visibility.Visible : Visibility.Collapsed;
        RefreshFilteredHints();
    }

    private void ServerSearch_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_api is null || _closed) return;
        RefreshServerList();
    }

    private void ClearServerFilters_Click(object sender, RoutedEventArgs e)
    {
        if (_busy || _closed) return;
        AgentServerSearchBox.Text = string.Empty;
        AgentServerSourceBox.SelectedIndex = 0;
        AgentServerSearchBox.Focus(FocusState.Programmatic);
    }

    private void SkillSearch_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_api is null || _closed) return;
        RefreshSkillList();
    }

    private void ClearSkillSearch_Click(object sender, RoutedEventArgs e)
    {
        if (_busy || _closed) return;
        AgentSkillSearchBox.Text = string.Empty;
        AgentSkillSearchBox.Focus(FocusState.Programmatic);
    }

    private void RefreshSkillList()
    {
        bool wasUpdating = _updating;
        _updating = true;
        string query = AgentSkillSearchBox.Text.Trim();
        var visibleSkills = _skills.Where(skill => query.Length == 0 ||
            skill.Name.Contains(query, StringComparison.OrdinalIgnoreCase) ||
            skill.Id.Contains(query, StringComparison.OrdinalIgnoreCase)).ToArray();
        AgentSkillList.ItemsSource = visibleSkills;
        AgentSkillList.SelectedItem = visibleSkills.FirstOrDefault(skill => skill.Id == _editingSkill?.Id);
        _updating = wasUpdating;
        UiLocalization.Bind(AgentSkillEmptyLabel, TextBlock.TextProperty,
            _skills.Length == 0 ? "尚无可用技能。" : "没有匹配的技能，请换个关键词或清除搜索。");
        AgentSkillEmptyLabel.Visibility = visibleSkills.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
        AgentSkillEmptyPanel.Visibility = AgentSkillEmptyLabel.Visibility;
        AgentClearSkillSearchButton.Visibility = visibleSkills.Length == 0 && _skills.Length > 0 && query.Length > 0
            ? Visibility.Visible : Visibility.Collapsed;
        RefreshFilteredHints();
    }

    private void RefreshFilteredHints()
    {
        // Visible list selection is a projection; hidden editors keep their stable identity and unsaved fields.
        // 可见列表选择只是展示投影；被隐藏的编辑器仍保留稳定身份及未保存字段。
        AgentServerFilteredHint.Visibility = _editingServerId is not null && !_servers.Any(server => server.Id == _editingServerId)
            ? Visibility.Visible : Visibility.Collapsed;
        AgentSkillFilteredHint.Visibility = _editingSkill is not null && !AgentSkillList.Items.OfType<AgentSkill>().Any(skill => skill.Id == _editingSkill.Id)
            ? Visibility.Visible : Visibility.Collapsed;
    }

    private void SelectServer(McpServerConfig server)
    {
        _editingServerId = server.Id;
        if ((_serverSourceIndex == 1 && !IsOfficial(server)) || (_serverSourceIndex == 2 && IsOfficial(server)))
        {
            _updating = true;
            _serverSourceIndex = AgentServerSourceBox.SelectedIndex = 0;
            _updating = false;
        }
        RefreshServerList();
        FillServer(server);
    }

    private void ServerSource_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_api is null || _updating || _closed) return;
        int selected = AgentServerSourceBox.SelectedIndex;
        if (selected < 0 || selected == _serverSourceIndex) return;
        _serverSourceIndex = selected;
        RefreshServerList();
    }

    private static void BindListSource(ContainerContentChangingEventArgs args, string name, string key)
    {
        if (!args.InRecycleQueue && args.ItemContainer.ContentTemplateRoot is FrameworkElement template &&
            template.FindName(name) is TextBlock label) UiLocalization.Bind(label, TextBlock.TextProperty, key);
    }

    private void ServerList_ContainerContentChanging(ListViewBase sender, ContainerContentChangingEventArgs e)
    {
        if (e.Item is McpServerConfig server) BindListSource(e, "AgentServerListSourceLabel", ServerSourceKey(server));
    }

    private void SkillList_ContainerContentChanging(ListViewBase sender, ContainerContentChangingEventArgs e)
    {
        if (e.Item is AgentSkill skill) BindListSource(e, "AgentSkillListSourceLabel", SkillSourceKey(skill));
    }

    private async void Refresh_Click(object sender, RoutedEventArgs e)
    {
        if (await ConfirmDiscardAsync()) await LoadAsync();
    }

    private async void Connect_Click(object sender, RoutedEventArgs e)
    {
        if (_busy || _closed) return;
        if (_toolDirty && !await ConfirmDiscardAsync()) return;
        SetBusy(true);
        try
        {
            var tools = await _api.RefreshMcpAsync(_conversationId, _lifetime.Token);
            if (!_closed)
            {
                AcceptTools(tools.Tools);
                AcceptConnections(tools.Connections);
                ReportConnectionOutcome(tools);
            }
        }
        catch (OperationCanceledException) when (_lifetime.IsCancellationRequested) { }
        catch (Exception error) { if (!_closed) Notice("MCP 连接失败，请核对程序和参数。", InfoBarSeverity.Error, error); }
        finally { if (!_closed) SetBusy(false); }
    }

    private void ConnectionErrors(string[] errors)
    {
        Notice("部分 MCP 服务连接失败，已保留可用工具。", InfoBarSeverity.Warning);
        _noticeDetails = " " + string.Join(" · ", errors);
        AgentStatusBar.Message += _noticeDetails;
    }

    private void ReportConnectionOutcome(AgentToolsResponse response, string? serverId = null)
    {
        if (response.Errors is { Length: > 0 }) ConnectionErrors(response.Errors);
        else if (response.Connections?.Any(item => item.State == "ready" && (serverId is null || item.ServerId == serverId)) == true)
            Notice("MCP 服务已连接。", InfoBarSeverity.Success);
        else Notice("MCP 尚未连接，请查看连接状态。", InfoBarSeverity.Warning);
    }

    private void AcceptTools(AgentTool[] tools)
    {
        string? name = _editingTool?.Name;
        _updating = true;
        AgentToolList.ItemsSource = tools;
        AgentToolList.SelectedItem = tools.FirstOrDefault(tool => tool.Name == name);
        _updating = false;
        FillTool(AgentToolList.SelectedItem as AgentTool);
    }

    private void AcceptSkills(AgentSkill[] skills, bool discardDraft)
    {
        string? id = _editingSkill?.Id;
        _updating = true;
        _skills = skills;
        _editingSkill = skills.FirstOrDefault(skill => skill.Id == id);
        if (_editingSkill?.Id != id)
        {
            _previewGeneration++;
            _preview?.Cancel();
            AgentSkillSourceLabel.Text = AgentSkillPreviewBox.Text = string.Empty;
        }
        _updating = false;
        RefreshSkillList();
        if (discardDraft || _editingSkill is null) FillSkillState(_editingSkill);
        else RenderSkillDiagnostics();
    }

    private void Preset_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_api is null) return;
        if (AgentPresetBox.SelectedItem is McpPreset preset)
            AgentPresetDescriptionLabel.Text = preset.Description + "\n" + preset.SourceUrl + "\n" + UiText.Get("添加后默认停用；启用后才会连接。");
        else AgentPresetDescriptionLabel.Text = _catalog.ReusedCapabilities.Length == 0 ? "" :
            UiText.Get("已有能力可直接复用：") + " " + string.Join(" · ", _catalog.ReusedCapabilities);
        SetBusy(_busy);
    }

    private async void AddPreset_Click(object sender, RoutedEventArgs e)
    {
        if (_config is null || _busy || AgentPresetBox.SelectedItem is not McpPreset preset) return;
        if (_serverDirty && !await ConfirmAsync("放弃未保存的修改？", "当前编辑尚未保存。", "放弃修改")) return;
        if (_closed) return;
        if (preset.AlreadyConfigured && _config.McpServers.FirstOrDefault(server => server.Id == preset.ConfiguredServerId) is { } existing)
        { SelectServer(existing); Notice("此预设已配置，已打开现有服务。", InfoBarSeverity.Informational); return; }
        SetBusy(true);
        try
        {
            var config = await _api.AddMcpPresetAsync(preset.Id, _config.Revision, _lifetime.Token);
            if (_closed) return;
            AcceptConfig(config, replaceDirectories: false);
            var added = config.McpServers.FirstOrDefault(server => server.PresetId == preset.Id || server.Id == preset.Server.Id);
            if (added is not null) SelectServer(added);
            Notice("预设已添加，当前保持停用。", InfoBarSeverity.Success);
            var refreshed = await _api.GetMcpCatalogAsync(_lifetime.Token);
            if (_closed) return;
            _catalog = refreshed; AgentPresetBox.ItemsSource = refreshed.Presets;
            AgentPresetBox.SelectedItem = refreshed.Presets.FirstOrDefault(item => item.Id == preset.Id);
        }
        catch (GatewayApiException error) when (error.StatusCode == HttpStatusCode.Conflict)
        {
            try { var current = await _api.GetConfigAsync(_lifetime.Token); if (!_closed) AcceptConfig(current, false); }
            catch (Exception) { if (!_closed) _config = null; }
            if (!_closed) Notice("配置已更新。编辑已保留，请核对列表后再次保存。", error: error);
        }
        catch (OperationCanceledException) when (_lifetime.IsCancellationRequested) { }
        catch (Exception error) { if (!_closed) Notice("预设添加失败，请刷新后重试。", InfoBarSeverity.Error, error); }
        finally { if (!_closed) SetBusy(false); }
    }

    private void AcceptConnections(McpConnectionDiagnostic[]? connections)
    { _connections = connections ?? []; RefreshConnectionStatus(); }

    private void RefreshConnectionStatus()
    {
        var state = _connections.FirstOrDefault(item => item.ServerId == _editingServerId);
        string key = state?.State switch { "ready" => "已连接", "connecting" => "正在连接", "error" => "连接失败",
            "auth-required" => "需要认证", _ => "未连接" };
        if (state?.Code is "MCP_ENV_MISSING" or "MCP_CONFIG_REQUIRED" or "MCP_COMMAND_NOT_FOUND") key = "未就绪";
        AgentConnectionStatusLabel.Text = string.Format(UiText.Get("连接状态：{0} · 工具：{1}"), UiText.Get(key), state?.ToolCount ?? 0) +
            (state?.Code is { } code ? " · " + code : "");
    }

    private async void Reconnect_Click(object sender, RoutedEventArgs e)
    {
        if (_busy || _closed || _editingServerId is not { } id) return;
        if (_serverDirty) { Notice("请先保存当前服务编辑，再连接。", InfoBarSeverity.Warning); return; }
        SetBusy(true);
        try
        {
            var response = await _api.ReconnectMcpAsync(id, _conversationId, _lifetime.Token);
            if (_closed) return;
            AcceptTools(response.Tools); AcceptConnections(response.Connections);
            ReportConnectionOutcome(response, id);
        }
        catch (OperationCanceledException) when (_lifetime.IsCancellationRequested) { }
        catch (Exception error) { if (!_closed) Notice("MCP 连接失败，请核对程序和参数。", InfoBarSeverity.Error, error); }
        finally { if (!_closed) SetBusy(false); }
    }

    private async void Disconnect_Click(object sender, RoutedEventArgs e)
    {
        if (_busy || _closed || _editingServerId is not { } id) return;
        SetBusy(true);
        try
        {
            var response = await _api.DisconnectMcpAsync(id, _lifetime.Token);
            if (!_closed) { AcceptConnections(response.Connections); Notice("MCP 服务已断开。", InfoBarSeverity.Success); }
        }
        catch (OperationCanceledException) when (_lifetime.IsCancellationRequested) { }
        catch (Exception error) { if (!_closed) Notice("MCP 断开失败，请重试。", InfoBarSeverity.Error, error); }
        finally { if (!_closed) SetBusy(false); }
    }

    private async void Server_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_updating || _closed) return;
        var selected = AgentServerList.SelectedItem as McpServerConfig;
        if (_serverDirty && !await ConfirmAsync("放弃未保存的修改？", "当前编辑尚未保存。", "放弃修改"))
        {
            _updating = true;
            AgentServerList.SelectedItem = _servers.FirstOrDefault(server => server.Id == _editingServerId);
            _updating = false;
            return;
        }
        if (!_closed) FillServer(selected);
    }

    private async void NewServer_Click(object sender, RoutedEventArgs e)
    {
        if (_serverDirty && !await ConfirmAsync("放弃未保存的修改？", "当前编辑尚未保存。", "放弃修改")) return;
        if (_closed) return;
        _updating = true; AgentServerList.SelectedItem = null; _updating = false;
        FillServer(null);
    }

    private void CancelServer_Click(object sender, RoutedEventArgs e)
    {
        if (_busy || _closed) return;
        FillServer(_config?.McpServers.FirstOrDefault(server => server.Id == _editingServerId));
    }

    private async void SaveServer_Click(object sender, RoutedEventArgs e)
    {
        if (_config is null) return;
        McpServerConfig edited;
        try
        {
            bool http = ServerTransport == "streamable-http";
            var args = http ? [] : JsonSerializer.Deserialize<string[]>(AgentServerArgsBox.Text) ?? throw new JsonException();
            if (!http && _browserSettingsEdited && ReadBrowserSettings(args) is { } browser) args = browser.Apply(args);
            if (args.Any(argument => argument is null || argument.Contains('\0')) || string.IsNullOrWhiteSpace(AgentServerIdBox.Text) ||
                string.IsNullOrWhiteSpace(AgentServerNameBox.Text) || (!http && string.IsNullOrWhiteSpace(AgentServerCommandBox.Text)) ||
                AgentServerIdBox.Text.Contains('\0') || AgentServerNameBox.Text.Contains('\0') || AgentServerCommandBox.Text.Contains('\0')) throw new JsonException();
            var previous = _config.McpServers.FirstOrDefault(server => server.Id == _editingServerId);
            string? cwd = string.IsNullOrWhiteSpace(AgentServerCwdBox.Text) ? null : AgentServerCwdBox.Text.Trim();
            if (!http && cwd is not null && (!Path.IsPathFullyQualified(cwd) || cwd.Contains('\0'))) throw new JsonException();
            string? url = http ? AgentServerUrlBox.Text.Trim() : null;
            if (http && (!Uri.TryCreate(url, UriKind.Absolute, out var address) || address.UserInfo.Length > 0 ||
                address.Query.Length > 0 || address.Fragment.Length > 0 || (address.Scheme != "https" && !(address.Scheme == "http" && address.IsLoopback)))) throw new JsonException();
            edited = new(AgentServerIdBox.Text.Trim(), AgentServerNameBox.Text.Trim(), http ? "" : AgentServerCommandBox.Text.Trim(), args,
                AgentServerEnabledBox.IsChecked == true, previous?.ProtocolVersion, previous?.DisabledTools ?? [], ServerTransport,
                http ? null : cwd, previous?.Env, http ? null : McpConfigurationInput.ReadReferences(AgentServerEnvRefsBox.Text, headers: false),
                url, http ? McpConfigurationInput.ReadReferences(AgentServerHeaderEnvBox.Text, headers: true) : null,
                http ? McpConfigurationInput.ReadAuthentication(AgentServerAuthBox.Text) : previous?.Auth, previous?.StartupTimeoutMs);
            if (_editingServerId is null && _config.McpServers.Any(server => server.Id == edited.Id))
                throw new JsonException();
        }
        catch (ArgumentException error)
        { Notice(error.Message, InfoBarSeverity.Error); return; }
        catch (JsonException)
        { Notice("请核对服务信息、URL、绝对路径及 JSON 环境变量引用。", InfoBarSeverity.Error); return; }
        var servers = _config.McpServers.Where(server => server.Id != _editingServerId).Append(edited).ToArray();
        if (await SaveAsync(servers, saveDirectories: false) && !_closed)
        {
            var saved = _config!.McpServers.FirstOrDefault(server => server.Id == edited.Id);
            if (saved is not null) SelectServer(saved);
        }
    }

    private async void DeleteServer_Click(object sender, RoutedEventArgs e)
    {
        if (_config is null || _editingServerId is not { } id || IsOfficial(_config.McpServers.FirstOrDefault(server => server.Id == id)) ||
            !await ConfirmAsync("删除服务？", "删除后，此服务不再连接。", "删除")) return;
        if (!_closed && await SaveAsync(_config.McpServers.Where(server => server.Id != id).ToArray(), saveDirectories: false)) FillServer(null);
    }

    private async void RestoreServer_Click(object sender, RoutedEventArgs e)
    {
        if (_config is null || _busy || _editingServerId is not { } id ||
            OfficialPresetFor(_config.McpServers.FirstOrDefault(server => server.Id == id)) is not { } preset ||
            !await ConfirmAsync("恢复默认？", "当前自定义服务配置将恢复为官方预设。", "恢复默认")) return;
        if (_closed) return;
        var restored = preset.Server with { Id = id, Origin = "official", PresetId = preset.Id, Overridden = null };
        var servers = _config.McpServers.Select(server => server.Id == id ? restored : server).ToArray();
        if (await SaveAsync(servers, saveDirectories: false) && !_closed &&
            _config!.McpServers.FirstOrDefault(server => server.Id == id) is { } saved) SelectServer(saved);
    }

    private ServerEditorState ReadEditor() => new(AgentServerIdBox.Text, AgentServerNameBox.Text, AgentServerCommandBox.Text,
        AgentServerArgsBox.Text, AgentServerEnabledBox.IsChecked == true, ServerTransport, AgentServerCwdBox.Text,
        AgentServerEnvRefsBox.Text, AgentServerUrlBox.Text, AgentServerHeaderEnvBox.Text, AgentServerAuthBox.Text,
        AgentBrowserModeBox.SelectedIndex, AgentBrowserVisibleBox.IsChecked == true, AgentBrowserEndpointBox.Text);

    private string ServerTransport => AgentServerTransportBox.SelectedIndex == 1 ? "streamable-http" : "stdio";
    private void RefreshTransportFields()
    { AgentStdioFields.Visibility = ServerTransport == "stdio" ? Visibility.Visible : Visibility.Collapsed;
        AgentHttpFields.Visibility = ServerTransport == "stdio" ? Visibility.Collapsed : Visibility.Visible;
        AgentStdioAdvancedFields.Visibility = AgentStdioFields.Visibility;
        AgentHttpAdvancedFields.Visibility = AgentHttpFields.Visibility; }
    private void Transport_SelectionChanged(object sender, SelectionChangedEventArgs e)
    { if (_api is null) return; RefreshTransportFields(); RefreshBrowserFields(); if (!_updating) { _serverDirty = ReadEditor() != _savedEditor; SetBusy(_busy); } }

    private void Server_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_updating || _api is null) return;
        if (ReferenceEquals(sender, AgentServerArgsBox) || ReferenceEquals(sender, AgentServerCommandBox) || ReferenceEquals(sender, AgentServerEnvRefsBox))
        {
            _updating = true;
            FillBrowserSettings();
            _updating = false;
        }
        _serverDirty = ReadEditor() != _savedEditor;
        SetBusy(_busy);
    }

    private void Server_TextChanging(TextBox sender, TextBoxTextChangingEventArgs e)
    {
        if (_updating || _api is null || _closed) return;
        if (ReferenceEquals(sender, AgentBrowserEndpointBox) && _browserSettings is null) return;
        // Refresh the save gate before text rendering; defer browser fields and visual layout to TextChanged.
        // 文字渲染前同步更新保存按钮；浏览器字段刷新及视觉布局仍由 TextChanged 完成。
        if (ReferenceEquals(sender, AgentBrowserEndpointBox) && _browserSettings is not null)
        {
            _browserSettingsEdited = true;
        }
        else if (ReferenceEquals(sender, AgentServerArgsBox) || ReferenceEquals(sender, AgentServerCommandBox) ||
            ReferenceEquals(sender, AgentServerEnvRefsBox))
        {
            _browserSettingsEdited = false;
        }
        _serverDirty = ReadEditor() != _savedEditor;
        RefreshServerSaveAvailability();
    }
    private void Server_EnabledChanged(object sender, RoutedEventArgs e)
    { if (!_updating && _api is not null) { _serverDirty = ReadEditor() != _savedEditor; SetBusy(_busy); } }

    private async void AddDirectory_Click(object sender, RoutedEventArgs e)
    {
        var picker = new FolderPicker();
        picker.FileTypeFilter.Add("*");
        WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
        var folder = await picker.PickSingleFolderAsync();
        if (_closed || folder is null || _directories.Contains(folder.Path, StringComparer.OrdinalIgnoreCase)) return;
        _directories.Add(folder.Path);
        _directoriesDirty = true;
        SetBusy(_busy);
    }

    private void Directory_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_api is null || _updating || _closed) return;
        SetBusy(_busy);
    }

    private void RemoveDirectory_Click(object sender, RoutedEventArgs e)
    {
        if (AgentDirectoryList.SelectedItem is not string directory) return;
        _directories.Remove(directory);
        _directoriesDirty = true;
        SetBusy(_busy);
    }

    private async void SaveDirectories_Click(object sender, RoutedEventArgs e)
    {
        if (_config is not null && await SaveAsync(_config.McpServers, saveDirectories: true) && !_closed)
        {
            SetBusy(true);
            try
            {
                var skills = await _api.GetSkillsAsync(_conversationId, _lifetime.Token);
                if (!_closed) AcceptSkills(skills, discardDraft: false);
            }
            catch (OperationCanceledException) when (_lifetime.IsCancellationRequested) { }
            catch (Exception error) { if (!_closed) Notice("工具列表读取失败，请重试。", InfoBarSeverity.Error, error); }
            finally { if (!_closed) SetBusy(false); }
        }
    }

    private async void Skill_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_updating || _closed) return;
        if (_skillDirty && !await ConfirmAsync("放弃未保存的修改？", "当前编辑尚未保存。", "放弃修改"))
        { _updating = true; AgentSkillList.SelectedItem = AgentSkillList.Items.OfType<AgentSkill>().FirstOrDefault(skill => skill.Id == _editingSkill?.Id); _updating = false; return; }
        _previewGeneration++;
        _preview?.Cancel();
        AgentSkillSourceLabel.Text = AgentSkillPreviewBox.Text = string.Empty;
        _editingSkill = AgentSkillList.SelectedItem as AgentSkill;
        FillSkillState(_editingSkill);
        RefreshFilteredHints();
        if (_editingSkill is not { } skill) return;
        AgentSkillSourceLabel.Text = skill.Source;
        if (skill.Status == "unavailable" || AgentTabs.SelectedIndex != 1) return;
        await LoadSkillPreviewAsync(skill);
    }

    private async void Tabs_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_api is null || _closed || _activeTabIndex == AgentTabs.SelectedIndex) return;
        _activeTabIndex = AgentTabs.SelectedIndex;
        if (AgentTabs.SelectedIndex != 1)
        {
            _previewGeneration++;
            _preview?.Cancel();
        }
        else if (_editingSkill is { Status: not "unavailable" } skill && AgentSkillPreviewBox.Text.Length == 0)
        {
            await LoadSkillPreviewAsync(skill);
        }
    }

    private async Task LoadSkillPreviewAsync(AgentSkill skill)
    {
        _preview?.Cancel();
        int generation = ++_previewGeneration;
        using var source = CancellationTokenSource.CreateLinkedTokenSource(_lifetime.Token);
        _preview = source;
        try
        {
            var preview = await _api.GetSkillAsync(skill.Id, _conversationId, source.Token);
            if (_closed || source.IsCancellationRequested || generation != _previewGeneration) return;
            AgentSkillSourceLabel.Text = preview.Source;
            ToolTipService.SetToolTip(AgentSkillSourceLabel, preview.Source);
            AgentSkillPreviewBox.Text = preview.Content;
        }
        catch (OperationCanceledException) when (source.IsCancellationRequested) { }
        catch (Exception error) { if (!_closed && generation == _previewGeneration) Notice("技能预览读取失败。", InfoBarSeverity.Error, error); }
        finally { if (ReferenceEquals(_preview, source)) _preview = null; }
    }

    private void FillSkillState(AgentSkill? skill)
    {
        _updating = true; _skillDirty = false;
        AgentSkillNameLabel.Text = skill?.Name ?? string.Empty;
        ToolTipService.SetToolTip(AgentSkillNameLabel, skill?.Name);
        UiLocalization.Bind(AgentSkillSourceTypeLabel, TextBlock.TextProperty, skill is null ? string.Empty : SkillSourceKey(skill));
        AgentSkillEnabledBox.IsChecked = skill is not null && !(_config?.DisabledSkills ?? []).Contains(skill.Id, StringComparer.Ordinal);
        RenderSkillDiagnostics();
        _updating = false; SetBusy(_busy);
    }

    private void RenderSkillDiagnostics()
    {
        var skill = _editingSkill;
        AgentSkillDiagnosticsLabel.Text = skill is null ? "" : string.Join("\n", (skill.Diagnostics ?? []).Select(item => item.Code + ": " + item.Message));
        if (skill?.Conflict is { } conflict) AgentSkillDiagnosticsLabel.Text += "\n" + UiText.Get(conflict.Preferred ?
            "此技能是同名技能的优先来源。" : "存在更高优先级的同名技能，请核对来源。");
    }

    private async void ImportSkill_Click(object sender, RoutedEventArgs e)
    {
        if (_closed || _busy || _config is null) return;
        var picker = new FolderPicker(); picker.FileTypeFilter.Add("*");
        WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
        var folder = await picker.PickSingleFolderAsync();
        if (_closed || folder is null) return;
        SetBusy(true);
        try
        {
            var imported = await _api.ImportSkillAsync(folder.Path, _lifetime.Token);
            var skills = await _api.GetSkillsAsync(_conversationId, _lifetime.Token);
            if (_closed) return;
            AcceptSkills(skills, discardDraft: false);
            Notice(imported.Reused ? "相同技能包已存在，已复用。" : "技能包已导入。", InfoBarSeverity.Success);
        }
        catch (OperationCanceledException) when (_lifetime.IsCancellationRequested) { }
        catch (Exception error) { if (!_closed) Notice("技能包导入失败，请核对目录与诊断。", InfoBarSeverity.Error, error); }
        finally { if (!_closed) SetBusy(false); }
    }

    private void Skill_EnabledChanged(object sender, RoutedEventArgs e)
    {
        if (_updating || _api is null || _editingSkill is not { } skill || _config is null) return;
        _skillDirty = AgentSkillEnabledBox.IsChecked != !(_config.DisabledSkills ?? []).Contains(skill.Id, StringComparer.Ordinal);
        SetBusy(_busy);
    }

    private async void SaveSkill_Click(object sender, RoutedEventArgs e)
    {
        if (_config is null || _busy || _editingSkill is not { } skill) return;
        var disabled = (_config.DisabledSkills ?? []).Where(id => id != skill.Id).ToList();
        if (AgentSkillEnabledBox.IsChecked != true) disabled.Add(skill.Id);
        if (await SaveAsync(_config.McpServers, false, disabled.ToArray()) && !_closed) FillSkillState(skill);
    }

    private void CancelSkill_Click(object sender, RoutedEventArgs e)
    {
        if (!_busy && !_closed) FillSkillState(_editingSkill);
    }

    private async void Tool_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_updating || _closed) return;
        if (_toolDirty && !await ConfirmAsync("放弃未保存的修改？", "当前编辑尚未保存。", "放弃修改"))
        { _updating = true; AgentToolList.SelectedItem = _editingTool; _updating = false; return; }
        FillTool(AgentToolList.SelectedItem as AgentTool);
    }

    private McpServerConfig? SelectedToolServer() => _editingTool is { Source: { } source, RawName: { } raw } &&
        source.StartsWith("mcp:", StringComparison.Ordinal) && !string.IsNullOrEmpty(raw)
        ? _config?.McpServers.FirstOrDefault(server => server.Id == source[4..]) : null;

    private void FillTool(AgentTool? tool)
    {
        _updating = true; _editingTool = tool; _toolDirty = false;
        AgentToolPreviewBox.Text = tool is null ? "" : tool.Name + "\n\n" + tool.Description + "\n\n" + tool.InputSchema.GetRawText();
        var server = SelectedToolServer();
        AgentToolEnabledBox.IsChecked = server is not null && !(server.DisabledTools ?? []).Contains(tool!.RawName, StringComparer.Ordinal);
        AgentExternalProgramLabel.Visibility = server is null ? Visibility.Collapsed : Visibility.Visible;
        _updating = false; SetBusy(_busy);
    }

    private void Tool_EnabledChanged(object sender, RoutedEventArgs e)
    {
        if (_updating || _api is null || SelectedToolServer() is not { } server) return;
        bool enabled = !(server.DisabledTools ?? []).Contains(_editingTool!.RawName, StringComparer.Ordinal);
        _toolDirty = AgentToolEnabledBox.IsChecked != enabled; SetBusy(_busy);
    }

    private async void SaveTool_Click(object sender, RoutedEventArgs e)
    {
        if (_config is null || _busy || SelectedToolServer() is not { } server || _editingTool?.RawName is not { } name) return;
        bool enabled = AgentToolEnabledBox.IsChecked == true;
        var disabled = (server.DisabledTools ?? []).Where(item => item != name).ToList();
        if (!enabled) disabled.Add(name);
        var replacement = server with { DisabledTools = disabled.ToArray() };
        var servers = _config.McpServers.Select(item => item.Id == server.Id ? replacement : item).ToArray();
        if (await SaveAsync(servers, saveDirectories: false) && !_closed) FillTool(_editingTool);
        // A conflict preserves this checkbox draft. An explicit second save uses the freshly loaded revision.
        // 发生冲突时保留复选框草稿；用户再次明确保存时使用新加载的版本号。
    }

    private Task<bool> ConfirmDiscardAsync() => !_serverDirty && !_directoriesDirty && !_toolDirty && !_skillDirty ? Task.FromResult(!_dialogOpen) :
        ConfirmAsync("放弃未保存的修改？", "当前编辑尚未保存。", "放弃修改");

    private async Task<bool> ConfirmAsync(string title, string content, string action)
    {
        if (_closed || _dialogOpen || AgentRoot.XamlRoot is null) return false;
        _dialogOpen = true;
        var dialog = new ContentDialog { XamlRoot = AgentRoot.XamlRoot, DefaultButton = ContentDialogButton.None,
            PrimaryButtonStyle = (Style)Application.Current.Resources["KynxaQuietButtonStyle"],
            CloseButtonStyle = (Style)Application.Current.Resources["KynxaQuietButtonStyle"] };
        UiLocalization.Bind(dialog, ContentDialog.TitleProperty, title);
        UiLocalization.Bind(dialog, ContentDialog.ContentProperty, content);
        UiLocalization.Bind(dialog, ContentDialog.PrimaryButtonTextProperty, action);
        UiLocalization.Bind(dialog, ContentDialog.CloseButtonTextProperty, "取消");
        try { return await dialog.ShowAsync() == ContentDialogResult.Primary; }
        finally { _dialogOpen = false; }
    }

    private async void Window_Closing(AppWindow sender, AppWindowClosingEventArgs e)
    {
        if (_allowClose || (!_serverDirty && !_directoriesDirty && !_toolDirty && !_skillDirty)) return;
        e.Cancel = true;
        if (await ConfirmDiscardAsync() && !_closed) { _allowClose = true; Close(); }
    }

    private void Window_Closed(object sender, WindowEventArgs e)
    {
        _closed = true;
        _lifetime.Cancel();
        _preview?.Cancel();
        UiText.LanguageChanged -= Language_Changed;
        AppWindow.Closing -= Window_Closing;
        if (_ownsApi && _api is IDisposable disposable) disposable.Dispose();
        _lifetime.Dispose();
    }
}
