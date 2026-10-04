using System.Diagnostics;
using System.Text.Json;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.Win32;

namespace KYNXA_Desktop;

public sealed partial class ToolManagementWindow
{
    private BrowserConnectionSettings? _browserSettings;
    private bool _browserSettingsEdited;

    private void FillBrowserSettings()
    {
        _browserSettingsEdited = false;
        try { _browserSettings = ServerTransport == "stdio" ? BrowserConnectionSettings.Read(AgentServerCommandBox.Text,
            JsonSerializer.Deserialize<string[]>(AgentServerArgsBox.Text) ?? [], BrowserEnvironmentNames()) : null; }
        catch (JsonException) { _browserSettings = null; }
        AgentBrowserModeBox.SelectedIndex = _browserSettings is null ? -1 : (int)_browserSettings.Mode;
        AgentBrowserVisibleBox.IsChecked = _browserSettings?.ShowWindow ?? true;
        AgentBrowserEndpointBox.Text = _browserSettings?.Endpoint ?? "";
        RefreshBrowserFields();
    }

    private BrowserConnectionSettings? ReadBrowserSettings(string[] args)
    {
        var browser = BrowserConnectionSettings.Read(AgentServerCommandBox.Text, args, BrowserEnvironmentNames());
        return browser is null ? null : browser with { Mode = (BrowserConnectionMode)AgentBrowserModeBox.SelectedIndex,
            ShowWindow = AgentBrowserVisibleBox.IsChecked == true, Endpoint = AgentBrowserEndpointBox.Text };
    }

    private void RefreshBrowserFields()
    {
        if (AgentBrowserFields is null) return;
        AgentBrowserFields.Visibility = _browserSettings is null || ServerTransport != "stdio" ? Visibility.Collapsed : Visibility.Visible;
        AgentBrowserModeBox.IsEnabled = _browserSettings?.HasEnvironmentConfiguration != true;
        var mode = (BrowserConnectionMode)AgentBrowserModeBox.SelectedIndex;
        AgentBrowserVisibleBox.Visibility = mode == BrowserConnectionMode.Independent ? Visibility.Visible : Visibility.Collapsed;
        AgentBrowserEndpointBox.Visibility = mode == BrowserConnectionMode.Remote ? Visibility.Visible : Visibility.Collapsed;
        bool chrome = _browserSettings?.Engine == "chrome-devtools";
        AgentBrowserSetupButton.Visibility = chrome && mode == BrowserConnectionMode.Existing ? Visibility.Visible : Visibility.Collapsed;
        AgentBrowserHintLabel.Text = UiText.Get(mode switch
        {
            BrowserConnectionMode.Existing when chrome => "连接你正在使用的 Chrome，保留已有登录状态。首次连接需在 Chrome 允许调试。",
            BrowserConnectionMode.Existing => "通过 Playwright 浏览器扩展连接现有 Chrome 或 Edge，保留已有登录状态。需安装官方扩展。",
            BrowserConnectionMode.Remote => "连接本机或远程 CDP 浏览器。远程浏览器不会自动拥有本机登录状态。",
            BrowserConnectionMode.Custom => "保留高级设置中的浏览器参数。",
            _ => "使用独立配置浏览网页；不会使用你日常浏览器的登录状态。"
        });
    }

    private IEnumerable<string> BrowserEnvironmentNames()
    {
        var previous = _config?.McpServers.FirstOrDefault(server => server.Id == _editingServerId);
        var references = JsonSerializer.Deserialize<Dictionary<string, string>>(AgentServerEnvRefsBox.Text);
        return (previous?.Env?.Keys ?? Enumerable.Empty<string>()).Concat(references?.Keys ?? Enumerable.Empty<string>());
    }

    private void BrowserMode_SelectionChanged(object sender, SelectionChangedEventArgs e) => BrowserSettingsChanged();
    private void BrowserOption_Changed(object sender, RoutedEventArgs e) => BrowserSettingsChanged();
    private void BrowserEndpoint_TextChanged(object sender, TextChangedEventArgs e) => BrowserSettingsChanged();
    private void BrowserSettingsChanged()
    {
        if (_api is null || _updating || _closed || _browserSettings is null) return;
        _browserSettingsEdited = true;
        RefreshBrowserFields();
        _serverDirty = ReadEditor() != _savedEditor;
        SetBusy(_busy);
    }

    private void BrowserSetup_Click(object sender, RoutedEventArgs e)
    {
        if (_closed || _busy) return;
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(@"Software\Microsoft\Windows\CurrentVersion\App Paths\chrome.exe")
                ?? Registry.LocalMachine.OpenSubKey(@"Software\Microsoft\Windows\CurrentVersion\App Paths\chrome.exe");
            string? path = key?.GetValue(null) as string;
            if (string.IsNullOrWhiteSpace(path) || !File.Exists(path.Trim('"'))) throw new IOException();
            var start = new ProcessStartInfo(path.Trim('"')) { UseShellExecute = true };
            start.ArgumentList.Add("chrome://inspect/#remote-debugging");
            Process.Start(start)?.Dispose();
        }
        catch { Notice("无法打开 Chrome 连接设置，请在浏览器打开 chrome://inspect/#remote-debugging。", InfoBarSeverity.Error); }
    }
}
