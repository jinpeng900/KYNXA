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

/// <summary>Edits gateway configuration and displays skills as text; reading never starts an MCP process.</summary>
public sealed partial class ToolManagementWindow : Window
{
    private readonly IAgentApi _api;
    private readonly Guid? _conversationId;
    private readonly CancellationTokenSource _lifetime = new();
    private readonly ObservableCollection<McpServerConfig> _servers = [];
    private readonly ObservableCollection<string> _directories = [];
    private AgentConfig? _config;
    private CancellationTokenSource? _preview;
    private int _previewGeneration;
    private string? _editingServerId;
    private bool _busy, _updating, _closed, _allowClose, _serverDirty, _directoriesDirty, _dialogOpen;
    private string? _noticeKey;
    private string _noticeDetails = string.Empty;
    private ServerEditorState? _savedEditor;
    private sealed record ServerEditorState(string Id, string Name, string Command, string Arguments, bool Enabled);

    public ToolManagementWindow(IAgentApi? api = null, Guid? conversationId = null)
    {
        InitializeComponent();
        _api = api ?? new AgentApiClient();
        _conversationId = conversationId;
        AgentServerList.ItemsSource = _servers;
        AgentDirectoryList.ItemsSource = _directories;
        UiLocalization.Bind(AgentServersTab, PivotItem.HeaderProperty, "MCP 服务");
        UiLocalization.Bind(AgentSkillsTab, PivotItem.HeaderProperty, "技能");
        UiLocalization.Bind(AgentToolsTab, PivotItem.HeaderProperty, "可用工具");
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

    public bool HasPendingChanges => _busy || _serverDirty || _directoriesDirty;

    public void CloseForOwner()
    {
        _allowClose = true;
        Close();
    }

    private async void AgentRoot_Loaded(object sender, RoutedEventArgs e) => await LoadAsync();

    private void Language_Changed(object? sender, EventArgs e)
    {
        if (_closed) return;
        if (!DispatcherQueue.HasThreadAccess) { DispatcherQueue.TryEnqueue(() => Language_Changed(sender, e)); return; }
        Title = UiText.Get("KYNXA · 工具与技能");
        if (_noticeKey is not null) AgentStatusBar.Message = UiText.Get(_noticeKey) + _noticeDetails;
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
        AgentAddDirectoryButton.IsEnabled = AgentRemoveDirectoryButton.IsEnabled = !value && _config is not null;
        AgentSaveDirectoriesButton.IsEnabled = !value && _config is not null && _directoriesDirty;
        AgentDeleteServerButton.IsEnabled = !value && _editingServerId is not null;
    }

    private void AcceptConfig(AgentConfig config, bool replaceDirectories)
    {
        if (config.Version != 1 || config.Revision < 0 || config.McpServers is null || config.SkillDirectories is null)
            throw new InvalidDataException(UiText.Get("工具配置版本不受支持。"));
        _config = config;
        _updating = true;
        _servers.Clear();
        foreach (var server in config.McpServers) _servers.Add(server);
        AgentServerList.SelectedItem = _servers.FirstOrDefault(server => server.Id == _editingServerId);
        if (replaceDirectories)
        {
            _directories.Clear();
            foreach (string directory in config.SkillDirectories) _directories.Add(directory);
            _directoriesDirty = false;
        }
        _updating = false;
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
            await Task.WhenAll(configTask, skillsTask, toolsTask);
            if (_closed) return;
            AcceptConfig(await configTask, replaceDirectories: true);
            AgentSkillList.ItemsSource = await skillsTask;
            var tools = await toolsTask;
            AgentToolList.ItemsSource = tools.Tools;
            FillServer(_servers.FirstOrDefault(server => server.Id == _editingServerId));
            AgentStatusBar.IsOpen = false;
            _noticeKey = null;
            if (tools.Errors is { Length: > 0 }) ConnectionErrors(tools.Errors);
        }
        catch (OperationCanceledException) when (_lifetime.IsCancellationRequested) { }
        catch (Exception error) { if (!_closed) Notice("工具列表读取失败，请重试。", InfoBarSeverity.Error, error); }
        finally { if (!_closed) SetBusy(false); }
    }

    private async Task<bool> SaveAsync(McpServerConfig[] servers, bool saveDirectories)
    {
        if (_config is null || _busy || _closed) return false;
        SetBusy(true);
        try
        {
            string[] directories = saveDirectories ? _directories.ToArray() : _config.SkillDirectories;
            var saved = await _api.SaveConfigAsync(new(1, _config.Revision, servers, directories), _lifetime.Token);
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
        _savedEditor = ReadEditor();
        _serverDirty = false;
        _updating = false;
        AgentDeleteServerButton.IsEnabled = !_busy && server is not null;
    }

    private async void Refresh_Click(object sender, RoutedEventArgs e)
    {
        if (await ConfirmDiscardAsync()) await LoadAsync();
    }

    private async void Connect_Click(object sender, RoutedEventArgs e)
    {
        if (_busy || _closed) return;
        SetBusy(true);
        try
        {
            var tools = await _api.RefreshMcpAsync(_conversationId, _lifetime.Token);
            if (!_closed)
            {
                AgentToolList.ItemsSource = tools.Tools;
                if (tools.Errors is { Length: > 0 }) ConnectionErrors(tools.Errors);
                else Notice("MCP 服务已连接。", InfoBarSeverity.Success);
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

    private async void Server_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_updating || _closed || AgentServerList.SelectedItem is not McpServerConfig selected) return;
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

    private async void SaveServer_Click(object sender, RoutedEventArgs e)
    {
        if (_config is null) return;
        McpServerConfig edited;
        try
        {
            var args = JsonSerializer.Deserialize<string[]>(AgentServerArgsBox.Text) ?? throw new JsonException();
            if (args.Any(argument => argument is null || argument.Contains('\0')) || string.IsNullOrWhiteSpace(AgentServerIdBox.Text) ||
                string.IsNullOrWhiteSpace(AgentServerNameBox.Text) || string.IsNullOrWhiteSpace(AgentServerCommandBox.Text) ||
                AgentServerIdBox.Text.Contains('\0') || AgentServerNameBox.Text.Contains('\0') || AgentServerCommandBox.Text.Contains('\0')) throw new JsonException();
            var previous = _config.McpServers.FirstOrDefault(server => server.Id == _editingServerId);
            edited = new(AgentServerIdBox.Text.Trim(), AgentServerNameBox.Text.Trim(), AgentServerCommandBox.Text.Trim(), args,
                AgentServerEnabledBox.IsChecked == true, previous?.ProtocolVersion);
            if (_editingServerId is null && _config.McpServers.Any(server => server.Id == edited.Id))
                throw new JsonException();
        }
        catch (Exception error) when (error is JsonException or ArgumentException)
        { Notice("请填写服务信息，并提供有效的 JSON 字符串数组参数。", InfoBarSeverity.Error); return; }
        var servers = _config.McpServers.Where(server => server.Id != _editingServerId).Append(edited).ToArray();
        if (await SaveAsync(servers, saveDirectories: false) && !_closed)
        {
            FillServer(edited);
            _updating = true;
            AgentServerList.SelectedItem = _servers.FirstOrDefault(server => server.Id == edited.Id);
            _updating = false;
        }
    }

    private async void DeleteServer_Click(object sender, RoutedEventArgs e)
    {
        if (_config is null || _editingServerId is not { } id || !await ConfirmAsync("删除服务？", "删除后，此服务不再连接。", "删除")) return;
        if (!_closed && await SaveAsync(_config.McpServers.Where(server => server.Id != id).ToArray(), saveDirectories: false)) FillServer(null);
    }

    private ServerEditorState ReadEditor() => new(AgentServerIdBox.Text, AgentServerNameBox.Text, AgentServerCommandBox.Text,
        AgentServerArgsBox.Text, AgentServerEnabledBox.IsChecked == true);

    private void Server_TextChanged(object sender, TextChangedEventArgs e) { if (!_updating && _api is not null) _serverDirty = ReadEditor() != _savedEditor; }
    private void Server_EnabledChanged(object sender, RoutedEventArgs e) { if (!_updating && _api is not null) _serverDirty = ReadEditor() != _savedEditor; }

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
                if (!_closed) AgentSkillList.ItemsSource = skills;
            }
            catch (OperationCanceledException) when (_lifetime.IsCancellationRequested) { }
            catch (Exception error) { if (!_closed) Notice("工具列表读取失败，请重试。", InfoBarSeverity.Error, error); }
            finally { if (!_closed) SetBusy(false); }
        }
    }

    private async void Skill_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        _previewGeneration++;
        _preview?.Cancel();
        AgentSkillSourceLabel.Text = AgentSkillPreviewBox.Text = string.Empty;
        if (AgentSkillList.SelectedItem is not AgentSkill skill || _closed) return;
        int generation = _previewGeneration;
        using var source = CancellationTokenSource.CreateLinkedTokenSource(_lifetime.Token);
        _preview = source;
        try
        {
            var preview = await _api.GetSkillAsync(skill.Id, _conversationId, source.Token);
            if (_closed || source.IsCancellationRequested || generation != _previewGeneration) return;
            AgentSkillSourceLabel.Text = preview.Source;
            AgentSkillPreviewBox.Text = preview.Content;
        }
        catch (OperationCanceledException) when (source.IsCancellationRequested) { }
        catch (Exception error) { if (!_closed && generation == _previewGeneration) Notice("技能预览读取失败。", InfoBarSeverity.Error, error); }
        finally { if (ReferenceEquals(_preview, source)) _preview = null; }
    }

    private void Tool_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (AgentToolList.SelectedItem is AgentTool tool)
            AgentToolPreviewBox.Text = tool.Name + "\n\n" + tool.Description + "\n\n" + tool.InputSchema.GetRawText();
    }

    private Task<bool> ConfirmDiscardAsync() => !_serverDirty && !_directoriesDirty ? Task.FromResult(!_dialogOpen) :
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
        if (_allowClose || (!_serverDirty && !_directoriesDirty)) return;
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
        if (_api is IDisposable disposable) disposable.Dispose();
        _lifetime.Dispose();
    }
}
