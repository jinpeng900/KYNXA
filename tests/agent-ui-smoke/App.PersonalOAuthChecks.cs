using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace AgentUiSmoke;

public partial class App
{
    private async Task CheckPersonalOAuthAsync()
    {
        await WaitAsync(() => _root.XamlRoot is not null && _api.Reads == 1 && Button("AgentRefreshButton").IsEnabled,
            "personal OAuth fixture loads through the injected configuration API");
        var servers = Element<ListView>("AgentServerList");
        servers.SelectedItem = servers.Items.Cast<McpServerConfig>().Single(item => item.Id == "personal-oauth");
        await SettleAsync();
        Check(Button("AgentOAuthLoginButton").Visibility == Visibility.Visible && Button("AgentOAuthLogoutButton").Visibility == Visibility.Visible
            && Element<TextBlock>("AgentConnectionStatusLabel").Text.Contains("需要认证"),
            "an account requiring personal OAuth exposes login and logout with truthful authentication state");
        Check(_api.OAuthStarts == 0 && _api.OAuthStatusReads == 0 && _api.OAuthLogouts == 0 && _api.Saves == 0 && _api.Connections == 0,
            "opening an account configuration neither authorizes, starts a server nor rewrites configuration");
        NativeUi.Invoke(Button("AgentOAuthLoginButton"));
        await WaitAsync(() => _api.OAuthStarts == 1 && Button("AgentOAuthLoginButton").IsEnabled
            && Element<InfoBar>("AgentStatusBar").IsOpen, "unsafe synthetic authorization URI is rejected inside the production UI");
        Check(_api.LastOAuthServerId == "personal-oauth" && _api.OAuthStatusReads == 0 && _api.Reconnects == 0,
            "a rejected authorization URI cannot launch polling or declare the MCP server connected");
        NativeUi.Invoke(Button("AgentOAuthLogoutButton"));
        await WaitAsync(() => _api.OAuthLogouts == 1 && Button("AgentRefreshButton").IsEnabled,
            "explicit logout completes through the selected server's account API");
        Check(_api.LastOAuthServerId == "personal-oauth" && Element<TextBlock>("AgentConnectionStatusLabel").Text == UiText.Get("已退出账号")
            && _api.Saves == 0 && _api.Connections == 0, "logout keeps the account identity and does not fabricate a connection or save");
        servers.SelectedItem = servers.Items.Cast<McpServerConfig>().Single(item => item.Id == "local-fixture");
        await SettleAsync();
        Check(Button("AgentOAuthLoginButton").Visibility == Visibility.Collapsed && Button("AgentOAuthLogoutButton").Visibility == Visibility.Collapsed,
            "ordinary local MCP services do not inherit another account's login controls");
        servers.SelectedItem = servers.Items.Cast<McpServerConfig>().Single(item => item.Id == "personal-oauth");
        UiText.Initialize("en");
        await SettleAsync();
        Check((string)Button("AgentOAuthLoginButton").Content == UiText.Get("登录个人账号")
            && (string)Button("AgentOAuthLogoutButton").Content == UiText.Get("退出账号") && !_window!.HasPendingChanges,
            "live language changes preserve the selected account and saved configuration");
    }
}
