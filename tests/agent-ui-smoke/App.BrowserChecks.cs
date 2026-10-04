using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace AgentUiSmoke;

public partial class App
{
    private async Task CheckBrowserSettingsAsync()
    {
        await WaitAsync(() => _root.XamlRoot is not null && _api.Reads == 1 && Button("AgentRefreshButton").IsEnabled,
            "production browser connection editor loads only isolated fake configuration");
        var list = Element<ListView>("AgentServerList");
        var mode = Element<ComboBox>("AgentBrowserModeBox");
        var visible = Element<CheckBox>("AgentBrowserVisibleBox");
        var endpoint = Element<TextBox>("AgentBrowserEndpointBox");
        var fields = Element<StackPanel>("AgentBrowserFields");
        var transport = Element<ComboBox>("AgentServerTransportBox");
        Check(_api.Saves == 0 && _api.Connections == 0 && _api.Reconnects == 0,
            "opening browser settings does not start browsers or connect MCP");
        list.SelectedItem = list.Items.Cast<McpServerConfig>().Single(server => server.Id == "playwright-fixture");
        await SettleAsync();
        Check(fields.Visibility == Visibility.Visible && mode.SelectedIndex == 0 && mode.IsEnabled &&
            visible.IsChecked == true && visible.Visibility == Visibility.Visible && endpoint.Visibility == Visibility.Collapsed,
            "independent Playwright mode exposes its visible window option without a remote endpoint");
        Check(mode.Items.Count == 4 && ((ComboBoxItem)mode.Items[1]).Content?.ToString() == "本机已登录浏览器" &&
            ((ComboBoxItem)mode.Items[2]).Content?.ToString() == "远程浏览器",
            "local independent, existing and remote choices have concise Chinese labels");
        Check(Element<TextBlock>("AgentBrowserHintLabel").Text.Contains("独立配置") &&
            !Element<Expander>("AgentServerAdvanced").IsExpanded,
            "the default browser editor explains its profile boundary with advanced arguments collapsed");
        await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "browser-independent-zh.png"));
        var original = _api.Config.McpServers.Single(server => server.Id == "playwright-fixture");
        visible.IsChecked = false;
        Check(_window!.HasPendingChanges && _api.Saves == 0 && _api.Config.McpServers.Single(server => server.Id == original.Id).Args.SequenceEqual(original.Args),
            "changing window visibility stays a draft and does not rewrite saved parameters");
        NativeUi.Invoke(Button("AgentSaveServerButton"));
        await WaitAsync(() => _api.Saves == 1 && !_window.HasPendingChanges, "window visibility changes require one explicit save");
        var saved = _api.Config.McpServers.Single(server => server.Id == original.Id);
        Check(saved.Args.Contains("--headless") && saved.Args.Contains("--profile-dir-name") && saved.Args.Contains("Profile 1") &&
            saved.Args.Contains(original.Args[3]) && saved.Args.Contains("--viewport-size") && saved.Args.Contains("1440,900") && !saved.Args.Contains("--isolated"),
            "saving visibility preserves explicit profile and unrelated browser parameters");

        mode.SelectedIndex = 1;
        await SettleAsync();
        Check(_api.Saves == 1 && _window.HasPendingChanges && visible.Visibility == Visibility.Collapsed &&
            endpoint.Visibility == Visibility.Collapsed && Button("AgentBrowserSetupButton").Visibility == Visibility.Collapsed &&
            Element<TextBlock>("AgentBrowserHintLabel").Text.Contains("官方扩展"),
            "existing Playwright mode is an unsaved extension connection with no headless or endpoint fields");
        NativeUi.Invoke(Button("AgentSaveServerButton"));
        await WaitAsync(() => _api.Saves == 2 && !_window.HasPendingChanges, "existing browser mode saves explicitly");
        saved = _api.Config.McpServers.Single(server => server.Id == original.Id);
        Check(saved.Args.Contains("--extension") && !saved.Args.Contains("--headless") && !saved.Args.Contains("--isolated") &&
            !saved.Args.Contains("--user-data-dir") && saved.Args.Contains("--viewport-size") && _api.Connections == 0 && _api.Reconnects == 0,
            "existing-session save removes incompatible launch flags while preserving unrelated parameters and never connecting automatically");

        mode.SelectedIndex = 2;
        await SettleAsync();
        Check(endpoint.Visibility == Visibility.Visible && visible.Visibility == Visibility.Collapsed,
            "remote mode exposes only the endpoint-specific field");
        NativeUi.SetText(endpoint, "file:///C:/Synthetic/NotABrowser");
        NativeUi.Invoke(Button("AgentSaveServerButton"));
        await SettleAsync();
        Check(_api.Saves == 2 && _window.HasPendingChanges && Element<InfoBar>("AgentStatusBar").Severity == InfoBarSeverity.Error,
            "invalid remote browser schemes never reach the configuration API");
        const string remoteEndpoint = "wss://browser.example.invalid/connect?token=FAKE_BROWSER_TOKEN";
        NativeUi.SetText(endpoint, remoteEndpoint);
        UiText.Initialize("en"); await SettleAsync();
        Check(mode.SelectedIndex == 2 && endpoint.Text == remoteEndpoint && _window.HasPendingChanges &&
            endpoint.Header?.ToString() == "Browser endpoint" && ((ComboBoxItem)mode.Items[2]).Content?.ToString() == "Remote browser" &&
            Element<TextBlock>("AgentBrowserHintLabel").Text.Contains("does not inherit"),
            "live English labels preserve the selected remote mode and unsaved endpoint");
        Check(NativeUi.Descendants<TextBlock>(mode).Any(text => text.Text == "Remote browser"),
            "the selected native browser caption updates language immediately");
        await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "browser-remote-draft-en.png"));
        NativeUi.Invoke(Button("AgentSaveServerButton"));
        await WaitAsync(() => _api.Saves == 3 && !_window.HasPendingChanges, "the remote endpoint saves only on explicit request");
        saved = _api.Config.McpServers.Single(server => server.Id == original.Id);
        Check(saved.Args.Contains("--cdp-endpoint") && saved.Args.Contains(remoteEndpoint) && !saved.Args.Contains("--extension") &&
            saved.Args.Contains("--viewport-size") && _api.Connections == 0 && _api.Reconnects == 0,
            "remote Playwright saves the exact endpoint and preserves unrelated options without starting a service");

        transport.SelectedIndex = 1; await SettleAsync();
        Check(fields.Visibility == Visibility.Collapsed && Element<StackPanel>("AgentHttpFields").Visibility == Visibility.Visible,
            "switching to an HTTP MCP transport immediately hides browser executable settings");
        transport.SelectedIndex = 0; await SettleAsync();
        Check(fields.Visibility == Visibility.Visible && mode.SelectedIndex == 2 && endpoint.Text == remoteEndpoint,
            "returning to stdio preserves the saved browser mode and endpoint");
        if (_window.HasPendingChanges) { NativeUi.Invoke(Button("AgentCancelServerButton")); await SettleAsync(); }

        list.SelectedItem = list.Items.Cast<McpServerConfig>().Single(server => server.Id == "chrome-fixture");
        await SettleAsync();
        Check(mode.SelectedIndex == 0 && visible.IsChecked == false, "the existing isolated headless Chrome preset is detected accurately");
        mode.SelectedIndex = 1; await SettleAsync();
        Check(Button("AgentBrowserSetupButton").Visibility == Visibility.Visible &&
            Element<TextBlock>("AgentBrowserHintLabel").Text.Contains("first connection"),
            "existing Chrome mode exposes a user-controlled setup action without launching it");
        NativeUi.Invoke(Button("AgentSaveServerButton"));
        await WaitAsync(() => _api.Saves == 4 && !_window.HasPendingChanges, "Chrome existing-session connection saves explicitly");
        saved = _api.Config.McpServers.Single(server => server.Id == "chrome-fixture");
        Check(saved.Args.Contains("--autoConnect") && saved.Args.Contains("--no-usage-statistics") &&
            !saved.Args.Contains("--headless") && !saved.Args.Contains("--isolated"),
            "Chrome existing mode uses autoConnect and retains diagnostic options");
        mode.SelectedIndex = 2; NativeUi.SetText(endpoint, "http://127.0.0.1:9222");
        NativeUi.Invoke(Button("AgentSaveServerButton"));
        await WaitAsync(() => _api.Saves == 5 && !_window.HasPendingChanges, "Chrome local CDP endpoint saves explicitly");
        saved = _api.Config.McpServers.Single(server => server.Id == "chrome-fixture");
        Check(saved.Args.Contains("--browserUrl") && saved.Args.Contains("http://127.0.0.1:9222") && !saved.Args.Contains("--autoConnect"),
            "Chrome remote mode selects the correct HTTP CDP flag");

        list.SelectedItem = list.Items.Cast<McpServerConfig>().Single(server => server.Id == "environment-fixture");
        await SettleAsync();
        Check(fields.Visibility == Visibility.Visible && mode.SelectedIndex == 3 && !mode.IsEnabled &&
            visible.Visibility == Visibility.Collapsed && endpoint.Visibility == Visibility.Collapsed,
            "environment-controlled browser connections are detected as custom and cannot be overwritten by the simplified mode picker");
        Check(_api.Config.McpServers.Single(server => server.Id == "environment-fixture").EnvRefs!["PLAYWRIGHT_MCP_CDP_ENDPOINT"] == "FAKE_BROWSER_ENDPOINT_ENV",
            "viewing environment-controlled mode preserves the reference name without reading its credential value");
        UiText.Initialize("zh-CN"); await SettleAsync();
        Check(mode.SelectedIndex == 3 && !mode.IsEnabled && Element<TextBlock>("AgentBrowserHintLabel").Text == "保留高级设置中的浏览器参数。",
            "custom browser diagnostics update language without changing environment mode");
        await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "browser-environment-custom-zh.png"));
        list.SelectedItem = list.Items.Cast<McpServerConfig>().Single(server => server.Id == "config-fixture");
        await SettleAsync();
        Check(mode.SelectedIndex == 3 && mode.IsEnabled && _api.Saves == 5,
            "a config-file browser keeps its advanced custom arguments on opening");
        mode.SelectedIndex = 1; NativeUi.Invoke(Button("AgentSaveServerButton")); await SettleAsync();
        Check(_api.Saves == 5 && _window.HasPendingChanges && Element<InfoBar>("AgentStatusBar").Severity == InfoBarSeverity.Error,
            "simplified mode changes cannot silently remove an advanced browser config file");
        NativeUi.Invoke(Button("AgentCancelServerButton")); await SettleAsync();
        Check(mode.SelectedIndex == 3 && !_window.HasPendingChanges && _api.Connections == 0 && _api.Reconnects == 0 && _api.PresetAdds == 0,
            "cancelling a custom mode edit restores its saved boundary without any browser or service execution");
        Element<Expander>("AgentServerAdvanced").IsExpanded = true;
        await SettleAsync();
        NativeUi.SetText(Element<TextBox>("AgentServerArgsBox"), "[\"-y\",\"@playwright/mcp\",null]");
        await SettleAsync();
        Check(fields.Visibility == Visibility.Collapsed && _window.HasPendingChanges && _api.Saves == 5 && _unhandled is null,
            "a null JSON argument remains an editable draft without crashing simplified browser controls");
        NativeUi.Invoke(Button("AgentSaveServerButton")); await SettleAsync();
        Check(_api.Saves == 5 && Element<InfoBar>("AgentStatusBar").Severity == InfoBarSeverity.Error,
            "invalid JSON argument members are rejected before a configuration write");
        NativeUi.Invoke(Button("AgentCancelServerButton")); await SettleAsync();
        Check(mode.SelectedIndex == 3 && !_window.HasPendingChanges && fields.Visibility == Visibility.Visible,
            "cancelling an invalid argument draft restores the saved browser projection");
    }
}
