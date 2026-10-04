using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace AgentUiSmoke;

public partial class App
{
    private async Task CheckOfficialToolsAsync()
    {
        await WaitAsync(() => _root.XamlRoot is not null && _api.Reads == 1 && Button("AgentRefreshButton").IsEnabled,
            "official and user configuration loads through the production window");
        var list = Element<ListView>("AgentServerList");
        var source = Element<ComboBox>("AgentServerSourceBox");
        Check(list.Items.Count == 2 && source.SelectedIndex == 0 && _api.Saves == 0 && _api.Connections == 0,
            "opening sources has no saves, MCP execution or duplicate list entries");
        var officialPath = Element<TextBlock>("AgentOfficialRootPath");
        var userPath = Element<TextBlock>("AgentUserRootPath");
        Check(officialPath.Text == _api.Config.OfficialToolsRoot && userPath.Text == _api.Config.UserToolsRoot &&
            officialPath.IsTextSelectionEnabled && userPath.IsTextSelectionEnabled,
            "source root paths come from the API and are readable text rather than editable fields");
        Check(Element<TextBlock>("AgentOfficialRootLabel").Text == "官方工具" && Element<TextBlock>("AgentUserRootLabel").Text == "用户工具",
            "the official read-only root and user migration root have distinct concise labels");
        var official = (McpServerConfig)list.Items[0];
        list.SelectedItem = official;
        await SettleAsync();
        Check(Element<TextBlock>("AgentServerSourceLabel").Text.Contains("官方工具") && Button("AgentDeleteServerButton").Visibility == Visibility.Collapsed &&
            !Button("AgentDeleteServerButton").IsEnabled && Button("AgentRestoreServerButton").Visibility == Visibility.Collapsed,
            "an unchanged official preset can be configured but cannot be deleted");
        var row = (FrameworkElement)list.ContainerFromItem(official);
        Check(NativeUi.ByName<TextBlock>(row, "AgentServerListSourceLabel").Text == "官方工具",
            "the realized official list row identifies its source");
        var enabled = Element<CheckBox>("AgentServerEnabledBox");
        Check(enabled.IsEnabled && enabled.IsChecked == false && Element<TextBox>("AgentServerIdBox").IsReadOnly,
            "official preset activation remains an explicit editable switch with stable identity");
        var command = Element<TextBox>("AgentServerCommandBox");
        Check(!command.IsReadOnly && command.Text == _api.OfficialBrowserDefault.Command,
            "official connection fields can be customized without modifying the read-only package path");
        source.SelectedIndex = 2;
        await SettleAsync();
        Check(list.Items.Count == 1 && ((McpServerConfig)list.Items[0]).Origin == "user" && _api.Saves == 0,
            "user filtering changes presentation without saving configuration");
        list.SelectedItem = list.Items[0]; await SettleAsync();
        Check(Button("AgentDeleteServerButton").Visibility == Visibility.Visible && Button("AgentDeleteServerButton").IsEnabled &&
            Element<TextBlock>("AgentServerSourceLabel").Text == "用户工具",
            "a custom user server keeps its edit and delete controls");
        source.SelectedIndex = 1;
        await SettleAsync();
        Check(list.Items.Count == 1 && ((McpServerConfig)list.Items[0]).Origin == "official",
            "official filtering excludes custom user servers");
        list.SelectedItem = list.Items[0]; await SettleAsync();
        enabled.IsChecked = true;
        var customized = _api.OfficialBrowserDefault with { Enabled = true, Overridden = true };
        _api.NextSaveResponse = _api.Config with { Revision = _api.Config.Revision + 1,
            McpServers = [customized, _api.Config.McpServers[1]] };
        NativeUi.Invoke(Button("AgentSaveServerButton"));
        await WaitAsync(() => _api.Saves == 1 && !_window!.HasPendingChanges, "official activation accepts the server-provided customization metadata");
        Check(_api.LastSaveRequest is { } first && first.McpServers.Length == 2 && first.McpServers.First(item => item.Id == official.Id).Enabled &&
            first.DisabledOfficialMcpServers?.SequenceEqual(["hidden-fixture"]) == true && first.SkillDirectories.SequenceEqual(_api.Config.SkillDirectories),
            "an official switch saves the full effective server array and preserves existing hidden presets and skill directories");
        Check(Element<TextBlock>("AgentServerSourceLabel").Text.Contains("官方 · 已自定义") &&
            Button("AgentRestoreServerButton").Visibility == Visibility.Visible && Button("AgentDeleteServerButton").Visibility == Visibility.Collapsed && _api.Connections == 0,
            "a saved official override stays official, offers restore, and does not auto-start a process");
        await SettleAsync();
        await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "official-customized-zh.png"));
        NativeUi.SetText(command, "fixture-browser-user-edit");
        var revised = customized with { Command = "fixture-browser-user-edit" };
        _api.NextSaveResponse = _api.Config with { Revision = _api.Config.Revision + 1,
            McpServers = [revised, _api.Config.McpServers[1]] };
        NativeUi.Invoke(Button("AgentSaveServerButton"));
        await WaitAsync(() => _api.Saves == 2 && !_window!.HasPendingChanges, "official program customization finishes an explicit save");
        Check(_api.LastSaveRequest?.McpServers.First(item => item.Id == official.Id).Command == "fixture-browser-user-edit" &&
            _api.Config.OfficialToolsRoot == officialPath.Text && _api.Connections == 0,
            "connection edits target the effective user override without changing or executing the official package");
        UiText.Initialize("en"); await SettleAsync();
        Check(Element<TextBlock>("AgentServerSourceLabel").Text.Contains("Official · Customized") &&
            Element<TextBlock>("AgentUserRootLabel").Text == "User tools" && ((ComboBoxItem)source.Items[1]).Content?.ToString() == "Official tools" &&
            command.Text == "fixture-browser-user-edit" && source.SelectedIndex == 1,
            "live language changes translate sources and keep selection, paths and custom fields");
        await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "official-customized-en.png"));
        NativeUi.Invoke(Button("AgentRestoreServerButton"));
        await WaitAsync(() => NativeUi.OpenDialog(_root) is not null, "restoring an official override requires an explicit native decision");
        NativeUi.InvokeDialogButton(NativeUi.OpenDialog(_root)!, primary: false);
        await WaitAsync(() => NativeUi.OpenDialog(_root) is null, "cancelling restore closes its confirmation");
        await Task.Delay(300);
        Check(_api.Saves == 2 && command.Text == "fixture-browser-user-edit", "cancelling restore preserves the customized server");
        NativeUi.Invoke(Button("AgentRestoreServerButton"));
        await WaitAsync(() => NativeUi.OpenDialog(_root) is not null, "official restore can be retried");
        _api.NextSaveResponse = _api.Config with { Revision = _api.Config.Revision + 1,
            McpServers = [_api.OfficialBrowserDefault, _api.Config.McpServers[1]] };
        NativeUi.InvokeDialogButton(NativeUi.OpenDialog(_root)!, primary: true);
        await WaitAsync(() => _api.Saves == 3 && !_window!.HasPendingChanges, "accepted restore receives the unchanged official default");
        Check(_api.LastSaveRequest is { } restored && restored.McpServers.Length == 2 &&
            restored.McpServers.First(item => item.Id == official.Id).Command == _api.OfficialBrowserDefault.Command &&
            !restored.McpServers.First(item => item.Id == official.Id).Enabled && command.Text == _api.OfficialBrowserDefault.Command &&
            Button("AgentRestoreServerButton").Visibility == Visibility.Collapsed && _api.Connections == 0,
            "restore sends the official preset under its stable ID, keeps other servers, and leaves it disabled");
        Element<Pivot>("AgentTabs").SelectedIndex = 1; await SettleAsync();
        var skills = Element<ListView>("AgentSkillList");
        string[] labels = ["Official tools", "User tools", "Project tools"];
        for (int index = 0; index < labels.Length; index++)
        {
            skills.SelectedItem = skills.Items[index];
            await SettleAsync();
            Check(Element<TextBlock>("AgentSkillSourceTypeLabel").Text == labels[index], "skill origin is shown as " + labels[index]);
            var skillRow = (FrameworkElement)skills.ContainerFromItem(skills.Items[index]);
            Check(NativeUi.ByName<TextBlock>(skillRow, "AgentSkillListSourceLabel").Text == labels[index], "the realized skill row identifies " + labels[index]);
        }
        Check(_api.Saves == 3 && _api.Connections == 0, "reading official, user and project skill sources has no execution or save side effects");
        await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "official-skill-sources-en.png"));
    }
}
