using KYNXA.Contracts;
using KYNXA_Desktop;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using MemoryUiSmoke;

namespace AgentUiSmoke;

public partial class App : Application
{
    private readonly string _directory = Path.Combine(Path.GetTempPath(), "kynxa-agent-ui-smoke-" + Guid.NewGuid().ToString("N"));
    private readonly List<string> _checks = [];
    private ToolManagementWindow? _window;
    private Window? _anchor;
    private FrameworkElement _root = null!;
    private FakeAgentApi _api = null!;
    private Exception? _unhandled;
    private string ResultPath => Path.Combine(_directory, "result.txt");

    public App()
    {
        Directory.CreateDirectory(_directory);
        Environment.SetEnvironmentVariable("KYNXA_DATA_HOME", Path.Combine(_directory, "Data"));
        Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", "http://127.0.0.1:1");
        File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-agent-ui-smoke-latest.txt"), ResultPath);
        InitializeComponent();
        UnhandledException += (_, args) => { _unhandled = args.Exception; args.Handled = true; File.AppendAllText(ResultPath, "\nUNHANDLED: " + args.Exception); };
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        File.WriteAllText(ResultPath, "RUNNING: isolated production tool management window\n");
        UiText.Initialize("zh-CN");
        _api = new FakeAgentApi(Path.Combine(_directory, "Skills"));
        _anchor = new Window { Content = new Grid() };
        _anchor.AppWindow.Hide();
        _window = new ToolManagementWindow(_api, Guid.NewGuid());
        _root = (FrameworkElement)_window.Content;
        _window.Activate();
        _ = RunAsync();
    }

    private T Element<T>(string name) where T : FrameworkElement => _root.FindName(name) as T ?? NativeUi.ByName<T>(_root, name);
    private Button Button(string name) => Element<Button>(name);
    private void Check(bool condition, string description)
    {
        if (!condition) throw new InvalidOperationException(description);
        _checks.Add("PASS " + description);
    }
    private async Task WaitAsync(Func<bool> condition, string description)
    {
        var deadline = DateTime.UtcNow.AddSeconds(15);
        while (!condition())
        {
            if (_unhandled is not null) throw new InvalidOperationException("Unhandled UI error", _unhandled);
            if (DateTime.UtcNow > deadline) throw new TimeoutException(description + $"; saves={_api.Saves}; directories={_api.Config.SkillDirectories.Length}; pending={_window?.HasPendingChanges}; " + Element<InfoBar>("AgentStatusBar").Message);
            await Task.Delay(25);
        }
        Check(true, description);
    }
    private static Task SettleAsync() => Task.Delay(130);

    private async Task RunAsync()
    {
        try
        {
            await WaitAsync(() => _root.XamlRoot is not null && _api.Reads == 1 && Button("AgentRefreshButton").IsEnabled, "production window loads injected configuration");
            Check(_window!.Title == "KYNXA · 工具与技能" && _window.ExtendsContentIntoTitleBar, "native localized caption uses the production title bar");
            Check(_window.AppWindow.Presenter is OverlappedPresenter { IsMinimizable: true, IsMaximizable: true, IsResizable: true }, "native minimize, maximize and resize remain enabled");
            Check(_api.Connections == 0 && _api.Saves == 0, "opening settings does not start MCP or save configuration");
            CheckRetryState();
            var tabs = Element<Pivot>("AgentTabs");
            Check(tabs.Items.Count == 2 && !Element<Expander>("AgentServerAdvanced").IsExpanded &&
                !Element<Expander>("AgentToolCatalogExpander").IsExpanded, "only MCP and skills are primary tabs; technical settings and tool catalog start collapsed");
            Check(Element<PivotItem>("AgentServersTab").Header?.ToString() == "MCP 服务" && Element<PivotItem>("AgentSkillsTab").Header?.ToString() == "技能", "Chinese management tabs render");
            var list = Element<ListView>("AgentServerList");
            list.SelectedItem = list.Items[0];
            await SettleAsync();
            var command = Element<TextBox>("AgentServerCommandBox");
            Check(command.Text == "example-mcp" && Element<TextBox>("AgentServerIdBox").IsReadOnly, "native server selection fills identity and command");
            Check(!NativeUi.IsVisible(Element<TextBox>("AgentServerArgsBox")) && !NativeUi.IsVisible(Element<TextBox>("AgentServerCwdBox")) &&
                !NativeUi.IsVisible(Element<TextBlock>("AgentServerTrustLabel")), "default MCP view keeps arguments, paths and technical notes out of the main form");
            await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "servers-simple-zh.png"));
            NativeUi.SetText(command, "cancelled-fixture-edit");
            await SettleAsync();
            Check(Button("AgentCancelServerButton").Visibility == Visibility.Visible && _window.HasPendingChanges, "server draft exposes a concise cancel action");
            NativeUi.Invoke(Button("AgentCancelServerButton"));
            await SettleAsync();
            Check(command.Text == "example-mcp" && !_window.HasPendingChanges && _api.Saves == 0, "cancelling a server draft restores saved fields without a gateway write");
            list.SelectedItem = null; await SettleAsync();
            Check(command.Text.Length == 0 && !Element<TextBox>("AgentServerIdBox").IsReadOnly, "clearing server selection removes the previous server editor");
            list.SelectedItem = list.Items[0]; await SettleAsync();
            Element<Expander>("AgentServerAdvanced").IsExpanded = true; await SettleAsync();
            NativeUi.SetText(command, "example-mcp-updated");
            NativeUi.SetText(Element<TextBox>("AgentServerArgsBox"), "not JSON");
            NativeUi.Invoke(Button("AgentSaveServerButton"));
            await SettleAsync();
            Check(_api.Saves == 0 && Element<InfoBar>("AgentStatusBar").IsOpen, "invalid arguments do not reach the API");
            NativeUi.SetText(Element<TextBox>("AgentServerArgsBox"), "[\"--stdio\",\"fixture\"]");
            Element<CheckBox>("AgentServerEnabledBox").IsChecked = true;
            _api.ConflictNextSave = true;
            NativeUi.Invoke(Button("AgentSaveServerButton"));
            await WaitAsync(() => _api.Reads == 2 && Button("AgentSaveServerButton").IsEnabled, "conflict reload finishes without automatically saving again");
            Check(_api.Saves == 1 && command.Text == "example-mcp-updated" && _window.HasPendingChanges, "configuration conflict preserves the editor");
            NativeUi.Invoke(Button("AgentSaveServerButton"));
            await WaitAsync(() => _api.Saves == 2 && !_window.HasPendingChanges, "explicit save accepts the refreshed revision");
            Check(_api.Config.McpServers[0].Command == "example-mcp-updated" && _api.Config.McpServers[0].Args.Length == 2, "native edit saves command and argument array");
            Check(_api.Config.McpServers[0].Enabled && _api.Connections == 0, "saving an enabled server does not automatically start its process");
            UiText.Initialize("en");
            await SettleAsync();
            Check(_window.Title == "KYNXA · Tools and skills" && Element<PivotItem>("AgentSkillsTab").Header?.ToString() == "Skills", "window caption and tabs switch to English immediately");
            Check(command.Text == "example-mcp-updated" && Element<TextBox>("AgentServerNameBox").Text == "Example MCP / 原样", "language switching preserves user configuration");
            Check(Element<TextBox>("AgentServerNameBox").Header?.ToString() == "Name", "server name field switches to English");
            Check(Element<TextBlock>("AgentServerTrustLabel").Text.Contains("outside the terminal sandbox"), "server editor identifies external execution before a process is enabled");
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "servers-en.png"));
            NativeUi.Invoke(Button("AgentDeleteServerButton"));
            await WaitAsync(() => NativeUi.OpenDialog(_root) is not null, "server deletion opens a native confirmation");
            var deleteDialog = NativeUi.OpenDialog(_root)!;
            Check(deleteDialog.DefaultButton == ContentDialogButton.None, "delete dialog has no automatic primary action");
            NativeUi.InvokeDialogButton(deleteDialog, primary: false);
            await WaitAsync(() => NativeUi.OpenDialog(_root) is null, "cancelled delete dialog finishes closing");
            await Task.Delay(350);
            Check(_api.Saves == 2 && _api.Config.McpServers.Length == 1, "cancelled server deletion does not save");
            NativeUi.Invoke(Button("AgentDeleteServerButton"));
            await WaitAsync(() => NativeUi.OpenDialog(_root) is not null, "server deletion can be explicitly retried");
            NativeUi.InvokeDialogButton(NativeUi.OpenDialog(_root)!, primary: true);
            await WaitAsync(() => _api.Saves == 3 && list.Items.Count == 0, "confirmed deletion updates configuration and native list");
            tabs.SelectedIndex = 1;
            await SettleAsync();
            Check(!Element<Expander>("AgentSkillDetailsExpander").IsExpanded && !Element<Expander>("AgentSkillDirectoriesExpander").IsExpanded,
                "skill source, diagnostics, content and external directories start collapsed");
            var skills = Element<ListView>("AgentSkillList");
            skills.SelectedItem = skills.Items[0];
            await WaitAsync(() => Element<TextBox>("AgentSkillPreviewBox").Text.Replace("\r\n", "\n").Replace('\r', '\n') == _api.PreviewText, "native skill selection reads its preview");
            Check(Element<TextBox>("AgentSkillPreviewBox").IsReadOnly && _api.Connections == 0 && _api.Saves == 3, "skill script text remains an unexecuted read-only preview");
            await Task.Delay(500);
            await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "skills-simple-en.png"));
            Element<CheckBox>("AgentSkillEnabledBox").IsChecked = false;
            Check(Button("AgentCancelSkillButton").Visibility == Visibility.Visible, "skill draft has the same concise cancel action");
            NativeUi.Invoke(Button("AgentCancelSkillButton"));
            await SettleAsync();
            Check(Element<CheckBox>("AgentSkillEnabledBox").IsChecked == true && !_window.HasPendingChanges && _api.Saves == 3,
                "cancelling a skill switch preserves gateway configuration");
            Element<Expander>("AgentSkillDetailsExpander").IsExpanded = true; await SettleAsync();
            UiText.Initialize("zh-CN");
            await SettleAsync();
            Check(Element<TextBox>("AgentSkillPreviewBox").Text.Replace("\r\n", "\n").Replace('\r', '\n') == _api.PreviewText, "skill source text survives language switching");
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "skills-zh.png"));
            var latePreview = new TaskCompletionSource<AgentSkillDetail>(TaskCreationOptions.RunContinuationsAsynchronously);
            _api.DelayedPreview = latePreview;
            skills.SelectedItem = skills.Items[1];
            await WaitAsync(() => _api.PreviewReads == 2, "second skill starts an independent preview request");
            var obsoleteToken = _api.LastPreviewToken;
            Check(Element<TextBox>("AgentSkillPreviewBox").Text.Length == 0, "selecting a new skill clears the old preview");
            _api.DelayedPreview = null;
            skills.SelectedItem = skills.Items[0];
            await WaitAsync(() => _api.PreviewReads == 3 && Element<TextBox>("AgentSkillPreviewBox").Text.Contains("Example skill"), "switching back renders the current skill");
            latePreview.SetResult(new("skill-two", "Second skill", "stale", "obsolete source", "OBSOLETE PREVIEW"));
            await SettleAsync();
            Check(obsoleteToken.IsCancellationRequested && !Element<TextBox>("AgentSkillPreviewBox").Text.Contains("OBSOLETE"), "cancelled late preview cannot overwrite the selected skill");
            var pagePreview = new TaskCompletionSource<AgentSkillDetail>(TaskCreationOptions.RunContinuationsAsynchronously);
            _api.DelayedPreview = pagePreview;
            int pageReads = _api.PreviewReads;
            skills.SelectedItem = skills.Items[1];
            await WaitAsync(() => _api.PreviewReads == pageReads + 1, "page switching fixture starts a delayed skill preview");
            var pageToken = _api.LastPreviewToken;
            tabs.SelectedIndex = 0; await SettleAsync();
            pagePreview.SetResult(new("skill-two", "Second skill", "stale page", "stale source", "OBSOLETE PAGE PREVIEW"));
            await SettleAsync();
            Check(pageToken.IsCancellationRequested && !Element<TextBox>("AgentSkillPreviewBox").Text.Contains("OBSOLETE PAGE"),
                "leaving the skill page cancels its read and discards late preview data");
            _api.DelayedPreview = null;
            tabs.SelectedIndex = 1;
            await WaitAsync(() => _api.PreviewReads == pageReads + 2 && Element<TextBox>("AgentSkillPreviewBox").Text.Contains("Example skill"),
                "returning to a skill page reloads a cancelled preview without changing selection");
            skills.SelectedItem = null; await SettleAsync();
            Check(Element<TextBlock>("AgentSkillNameLabel").Text.Length == 0 && Element<TextBox>("AgentSkillPreviewBox").Text.Length == 0 &&
                !Element<CheckBox>("AgentSkillEnabledBox").IsEnabled, "empty skill selection clears details and disables editing");
            skills.SelectedItem = skills.Items[0];
            await WaitAsync(() => Element<TextBox>("AgentSkillPreviewBox").Text.Contains("Example skill"), "skill can be selected again after clearing the editor");
            Element<Expander>("AgentSkillDirectoriesExpander").IsExpanded = true; await SettleAsync();
            var directories = Element<ListView>("AgentDirectoryList");
            directories.SelectedItem = directories.Items[0];
            NativeUi.Invoke(Button("AgentRemoveDirectoryButton"));
            Check(_api.Config.SkillDirectories.Length == 1 && _window.HasPendingChanges, "directory edits remain unsaved in memory");
            NativeUi.Invoke(Button("AgentSaveDirectoriesButton"));
            await WaitAsync(() => _api.Saves == 4 && _api.Config.SkillDirectories.Length == 0 && !_window.HasPendingChanges, "explicit directory save updates the gateway configuration");
            tabs.SelectedIndex = 0;
            Element<Expander>("AgentToolCatalogExpander").IsExpanded = true;
            await SettleAsync();
            var tools = Element<ListView>("AgentToolList");
            tools.SelectedItem = tools.Items[0];
            Check(Element<TextBox>("AgentToolPreviewBox").Text.Contains("file.read") && Element<TextBox>("AgentToolPreviewBox").IsReadOnly, "tool parameters are visible without execution");
            NativeUi.Invoke(Button("AgentConnectButton"));
            await WaitAsync(() => _api.Connections == 1 && Button("AgentConnectButton").IsEnabled, "explicit connection button alone refreshes MCP");
            _api.ConnectionErrors = ["example:MCP_CONNECTION_FAILED"];
            NativeUi.Invoke(Button("AgentConnectButton"));
            await WaitAsync(() => _api.Connections == 2 && Button("AgentConnectButton").IsEnabled, "partial MCP failure finishes the explicit refresh");
            Check(tools.Items.Count == 1 && Element<InfoBar>("AgentStatusBar") is { Severity: InfoBarSeverity.Warning } status &&
                status.Message.Contains("example:MCP_CONNECTION_FAILED"), "partial MCP failures retain tools and expose the safe error code");
            await CheckApprovalAsync();
            await CheckMcpToolsAsync();
            await CheckResultAsync();
            await CheckExpandedManagementAsync();
            await CheckStorageRowsAsync();
            tabs.SelectedIndex = 1;
            await SettleAsync();
            var closingPreview = new TaskCompletionSource<AgentSkillDetail>(TaskCreationOptions.RunContinuationsAsynchronously);
            _api.DelayedPreview = closingPreview;
            int readsBeforeClose = _api.PreviewReads;
            skills.SelectedItem = skills.Items[1];
            await WaitAsync(() => _api.PreviewReads == readsBeforeClose + 1, "closing fixture has a pending preview");
            var closingToken = _api.LastPreviewToken;
            _window.CloseForOwner();
            closingPreview.SetResult(new("skill-two", "Second skill", "late close", "late source", "LATE CLOSED PREVIEW"));
            await SettleAsync();
            Check(_api.Disposed && closingToken.IsCancellationRequested, "native closing cancels reads and disposes the injected API");
            Check(_unhandled is null, "no unhandled UI errors occurred");
            File.AppendAllText(ResultPath, string.Join("\n", _checks) + $"\nPASS: {_checks.Count} native tool UI checks.\nPreviews: {_directory}");
        }
        catch (Exception error)
        {
            Environment.ExitCode = 1;
            File.AppendAllText(ResultPath, "FAIL after " + _checks.Count + " checks: " + error);
        }
        finally
        {
            _window?.CloseForOwner();
            _anchor?.Close();
        }
    }

    private async Task CheckApprovalAsync()
    {
        var activity = new ToolActivity("fixture-call", "terminal.exec", System.Text.Json.JsonSerializer.SerializeToElement(new
            { command = "echo literal fixture", path = "example.txt" }), "approval-required", "Fixture requested command", ApprovalId: Guid.NewGuid(),
            OutsideWorkspace: true, Sandbox: "appcontainer", WorkspaceRoot: Path.Combine(_directory, "Mounted"));
        var dialog = ToolApprovalDialog.Create(_root.XamlRoot, activity);
        var decision = dialog.ShowAsync();
        await WaitAsync(() => NativeUi.OpenDialog(_root) is not null, "production approval dialog opens for the requested operation");
        await SettleAsync();
        Check(NativeUi.ByName<TextBlock>(dialog, "ToolApprovalName").Text == "terminal.exec", "approval displays the actual tool name");
        Check(NativeUi.ByName<TextBlock>(dialog, "ToolApprovalWorkspace").Text == activity.WorkspaceRoot, "approval displays the actual workspace");
        Check(NativeUi.ByName<TextBox>(dialog, "ToolApprovalArguments").Text.Contains("echo literal fixture"), "approval displays the actual requested parameters");
        Check(dialog.DefaultButton == ContentDialogButton.None && NativeUi.ByName<TextBox>(dialog, "ToolApprovalArguments").IsReadOnly &&
            NativeUi.ByName<TextBlock>(dialog, "ToolApprovalScope").Text.Contains("超出"), "approval has no automatic action and describes external scope");
        GC.Collect();
        await SettleAsync();
        UiText.Initialize("en");
        await SettleAsync();
        Check(dialog.PrimaryButtonText == "Approve once" && dialog.CloseButtonText == "Deny" &&
            NativeUi.ByName<TextBlock>(dialog, "ToolApprovalScope").Text.Contains("outside"), "live approval labels change language after garbage collection");
        await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "approval-en.png"));
        NativeUi.InvokeDialogButton(dialog, primary: false);
        Check(await decision == ContentDialogResult.None, "closing approval is a denial");
        dialog = ToolApprovalDialog.Create(_root.XamlRoot, activity);
        decision = dialog.ShowAsync();
        await WaitAsync(() => NativeUi.OpenDialog(_root) is not null, "approval can await a distinct explicit decision");
        NativeUi.InvokeDialogButton(dialog, primary: true);
        Check(await decision == ContentDialogResult.Primary, "only explicit primary invocation approves once");
        dialog = ToolApprovalDialog.Create(_root.XamlRoot, activity);
        decision = dialog.ShowAsync();
        await WaitAsync(() => NativeUi.OpenDialog(_root) is not null, "approval can be cancelled with its stream");
        dialog.Hide();
        Check(await decision == ContentDialogResult.None, "cancelled approval cannot become approved");
        await Task.Delay(250);
        activity = activity with { Name = "mcp.example.raw.tool", Arguments = System.Text.Json.JsonSerializer.SerializeToElement(new
            { arguments = new { path = "business.txt" }, policy = new { reason = "Trusted external process" } }) };
        dialog = ToolApprovalDialog.Create(_root.XamlRoot, activity);
        decision = dialog.ShowAsync();
        await WaitAsync(() => NativeUi.OpenDialog(_root) is not null, "MCP approval opens with policy wrapper");
        Check(NativeUi.ByName<TextBlock>(dialog, "ToolApprovalExternalProgram").Text.Contains("outside the terminal sandbox") &&
            NativeUi.ByName<TextBlock>(dialog, "ToolApprovalReason").Text == "Trusted external process", "MCP approval identifies trusted external execution and the actual policy reason");
        Check(NativeUi.ByName<TextBox>(dialog, "ToolApprovalArguments").Text.Contains("business.txt") &&
            NativeUi.ByName<TextBox>(dialog, "ToolApprovalArguments").Text.Contains("policy"), "approval displays the complete immutable wrapper alongside the business arguments");
        NativeUi.InvokeDialogButton(dialog, primary: false); await decision; await Task.Delay(250);
    }

    private async Task CheckMcpToolsAsync()
    {
        _api.InstallMcpTools();
        int reads = _api.Reads, saves = _api.Saves, connections = _api.Connections;
        Element<Pivot>("AgentTabs").SelectedIndex = 0;
        Element<Expander>("AgentToolCatalogExpander").IsExpanded = true;
        NativeUi.Invoke(Button("AgentRefreshButton"));
        await WaitAsync(() => _api.Reads == reads + 1 && Button("AgentRefreshButton").IsEnabled, "refresh discovers individually configurable MCP tools without connecting");
        var tools = Element<ListView>("AgentToolList");
        tools.SelectedItem = tools.Items[0]; await SettleAsync();
        Check(!Element<CheckBox>("AgentToolEnabledBox").IsEnabled && !Button("AgentSaveToolButton").IsEnabled, "builtin tools remain outside the MCP configuration editor");
        tools.SelectedItem = tools.Items[1]; await SettleAsync();
        var enabled = Element<CheckBox>("AgentToolEnabledBox");
        Check(enabled.IsChecked == true && enabled.IsEnabled && Element<TextBlock>("AgentExternalProgramLabel").Visibility == Visibility.Visible,
            "MCP tool selection shows its switch and external program scope");
        enabled.IsChecked = false;
        Check(_api.Saves == saves && _window!.HasPendingChanges && Button("AgentSaveToolButton").IsEnabled, "tool switch is an unsaved explicit draft");
        _api.ConflictNextSave = true;
        NativeUi.Invoke(Button("AgentSaveToolButton"));
        await WaitAsync(() => _api.Reads == reads + 2 && Button("AgentSaveToolButton").IsEnabled, "MCP tool conflict reloads configuration without overwriting the draft");
        Check(enabled.IsChecked == false && _api.Saves == saves + 1, "tool conflict keeps the requested disabled state and does not auto-save");
        NativeUi.Invoke(Button("AgentSaveToolButton"));
        await WaitAsync(() => _api.Saves == saves + 2 && !_window!.HasPendingChanges, "explicit MCP tool save uses the refreshed revision");
        Check(_api.Config.McpServers.Single().DisabledTools!.Single() == "raw.tool", "MCP tool disable preserves the exact raw tool name including dots");
        enabled.IsChecked = true; NativeUi.Invoke(Button("AgentSaveToolButton"));
        await WaitAsync(() => _api.Saves == saves + 3 && !_window!.HasPendingChanges, "MCP tool can be explicitly enabled again");
        Check(_api.Config.McpServers.Single().DisabledTools!.Length == 0 && _api.Connections == connections, "tool setting saves never start an external process");
        await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "mcp-tools-en.png"));
    }

    private async Task CheckResultAsync()
    {
        var activity = new ToolActivity("fixture-result", "mcp.example.raw.tool", null, "completed", "Fixture result", ResultRef: _api.ResultReference);
        using (var viewer = new ToolResultDialog(_root.XamlRoot, _api, Guid.NewGuid(), activity))
        {
            var shown = viewer.ShowAsync();
            await WaitAsync(() => _api.ResultPages == 1 && NativeUi.ByName<TextBox>(viewer.Dialog, "ToolResultText").Text.Length == 16000,
                "result dialog initially loads only the first bounded text page");
            Check(_api.ResultReads == 0 && viewer.Dialog.DefaultButton == ContentDialogButton.None, "result opening does not fetch media or perform an automatic action");
            Check(NativeUi.ByName<TextBox>(viewer.Dialog, "ToolResultText").Resources["TextControlBorderBrushFocused"] is
                Microsoft.UI.Xaml.Media.SolidColorBrush { Color.R: 136, Color.G: 136, Color.B: 136 }, "result viewer uses the neutral gray focus border");
            NativeUi.Invoke(NativeUi.ByName<Button>(viewer.Dialog, "ToolResultMore"));
            await WaitAsync(() => _api.ResultPages == 2 && NativeUi.ByName<TextBox>(viewer.Dialog, "ToolResultText").Text.Contains("尾文"), "explicit next section appends the remaining result text");
            Check(_api.LastResultOffset == 16000 && NativeUi.ByName<Button>(viewer.Dialog, "ToolResultMore").Visibility == Visibility.Collapsed,
                "result pagination advances exactly and finishes at the last character");
            NativeUi.Invoke(NativeUi.ByName<Button>(viewer.Dialog, "ToolResultMedia"));
            await WaitAsync(() => _api.ResultReads == 1 && NativeUi.ByName<Button>(viewer.Dialog, "ToolResultMedia").IsEnabled,
                "only the explicit media action reads the complete public result");
            var resources = NativeUi.ByName<StackPanel>(viewer.Dialog, "ToolResultResources");
            Check(resources.Children.OfType<Image>().Any(image => image.Source is not null), "supported image content renders from the fetched result");
            Check(resources.Children.OfType<TextBox>().Any(text => text.Text.Contains("resource://fixture/report")) &&
                resources.Children.OfType<TextBlock>().Any(text => text.Text.Contains("cannot be previewed")), "resource URIs stay literal and unsupported media receives an explicit explanation");
            UiText.Initialize("zh-CN"); await SettleAsync();
            Check(viewer.Dialog.Title?.ToString() == "工具结果详情" && NativeUi.ByName<TextBox>(viewer.Dialog, "ToolResultText").Text.Contains("neverExecuted"),
                "result dialog localizes while preserving fetched text");
            await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "result-zh.png"));
            viewer.Dialog.Hide(); await shown; await Task.Delay(250);
        }
        var delayed = new TaskCompletionSource<ToolResultPage>(TaskCreationOptions.RunContinuationsAsynchronously);
        _api.DelayedResult = delayed;
        using var cancellation = new CancellationTokenSource();
        using var lateViewer = new ToolResultDialog(_root.XamlRoot, _api, Guid.NewGuid(), activity, cancellation.Token);
        var lateShown = lateViewer.ShowAsync();
        await WaitAsync(() => _api.ResultPages == 3, "second result viewer starts an independent read");
        var token = _api.LastResultToken;
        cancellation.Cancel(); await lateShown;
        delayed.SetResult(new(_api.ResultReference.Id, "OBSOLETE RESULT", 15, 0, 15, false, _api.ResultReference));
        await SettleAsync();
        Check(token.IsCancellationRequested && !NativeUi.ByName<TextBox>(lateViewer.Dialog, "ToolResultText").Text.Contains("OBSOLETE"),
            "closing or switching conversation cancels the result read and discards late data");
        _api.DelayedResult = null;
    }

    private async Task CheckExpandedManagementAsync()
    {
        UiText.Initialize("en");
        var tabs = Element<Pivot>("AgentTabs"); tabs.SelectedIndex = 0; await SettleAsync();
        var presets = Element<ComboBox>("AgentPresetBox");
        Check(presets.Items.Count == 1 && _api.PresetAdds == 0, "preset choices come only from the gateway catalog");
        presets.SelectedIndex = 0; NativeUi.Invoke(Button("AgentAddPresetButton"));
        await WaitAsync(() => _api.PresetAdds == 1 && Button("AgentAddPresetButton").IsEnabled, "one explicit action adds the pinned preset");
        int connections = _api.Connections, serverCount = _api.Config.McpServers.Length;
        Check(!_api.Config.McpServers.Single(server => server.Id == "browser-fixture").Enabled && _api.Reconnects == 0,
            "new preset remains disabled without starting a process");
        NativeUi.Invoke(Button("AgentAddPresetButton")); await SettleAsync();
        Check(_api.PresetAdds == 1 && _api.Config.McpServers.Length == serverCount, "adding the same preset selects its existing server without duplication");
        int presetSaves = _api.Saves;
        NativeUi.SetText(Element<TextBox>("AgentServerNameBox"), "Browser fixture updated");
        NativeUi.Invoke(Button("AgentSaveServerButton"));
        await WaitAsync(() => _api.Saves == presetSaves + 1 && !_window!.HasPendingChanges, "preset remains editable through the existing server editor");
        Check(_api.Config.McpServers.Single(server => server.Id == "browser-fixture").StartupTimeoutMs == 60000,
            "editing a pinned preset preserves its cold-start allowance");
        NativeUi.Invoke(Button("AgentNewServerButton")); await SettleAsync();
        Element<Expander>("AgentToolCatalogExpander").IsExpanded = false;
        Element<Expander>("AgentServerAdvanced").IsExpanded = true;
        Element<ComboBox>("AgentServerTransportBox").SelectedIndex = 1;
        NativeUi.SetText(Element<TextBox>("AgentServerNameBox"), "HTTP fixture / 原样");
        NativeUi.SetText(Element<TextBox>("AgentServerUrlBox"), "https://mcp.test.invalid/service?token=forbidden-fixture");
        NativeUi.SetText(Element<TextBox>("AgentServerHeaderEnvBox"), "{\"Authorization\":\"FIXTURE_AUTHORIZATION\"}");
        NativeUi.SetText(Element<TextBox>("AgentServerAuthBox"), "{\"type\":\"bearer-env\",\"tokenEnv\":\"FIXTURE_TOKEN\"}");
        int saves = _api.Saves;
        NativeUi.Invoke(Button("AgentSaveServerButton")); await SettleAsync();
        Check(_api.Saves == saves && Element<InfoBar>("AgentStatusBar").Severity == InfoBarSeverity.Error,
            "credential-bearing URL query does not reach the gateway");
        NativeUi.SetText(Element<TextBox>("AgentServerUrlBox"), "https://mcp.test.invalid/service");
        NativeUi.Invoke(Button("AgentSaveServerButton"));
        await WaitAsync(() => _api.Saves == saves + 1 && !_window!.HasPendingChanges, "HTTP server saves with environment references");
        var remote = _api.Config.McpServers.Single(server => server.Name == "HTTP fixture / 原样");
        Check(remote.Command == "" && remote.Args.Length == 0 && remote.Transport == "streamable-http" &&
            remote.Auth?.TokenEnv == "FIXTURE_TOKEN" && remote.HeaderEnv!["Authorization"] == "FIXTURE_AUTHORIZATION",
            "HTTP configuration keeps names only and does not require a local executable");
        Check(Element<StackPanel>("AgentStdioFields").Visibility == Visibility.Collapsed && Element<StackPanel>("AgentHttpFields").Visibility == Visibility.Visible,
            "transport selection shows only the applicable fields");
        UiText.Initialize("zh-CN"); await SettleAsync();
        Check(Element<TextBox>("AgentServerHeaderEnvBox").Header?.ToString() == "请求头环境变量引用（JSON 映射）" &&
            Element<TextBox>("AgentServerUrlBox").Text == remote.Url, "expanded connection labels localize without changing URL or references");
        Check(NativeUi.Descendants<TextBlock>(Element<ComboBox>("AgentServerTransportBox")).Any(text => text.Text == "HTTP 服务"),
            "selected transport caption changes language immediately");
        Element<CheckBox>("AgentServerEnabledBox").IsChecked = true; NativeUi.Invoke(Button("AgentSaveServerButton"));
        await WaitAsync(() => _api.Saves == saves + 2 && Button("AgentReconnectButton").IsEnabled, "enabled saved server exposes explicit connect");
        NativeUi.Invoke(Button("AgentReconnectButton"));
        await WaitAsync(() => _api.Reconnects == 1 && Button("AgentReconnectButton").IsEnabled, "explicit per-server connect updates its diagnostic state");
        Check(Element<TextBlock>("AgentConnectionStatusLabel").Text.Contains("已连接"), "ready state and tool count are visible");
        NativeUi.Invoke(Button("AgentDisconnectButton"));
        await WaitAsync(() => _api.Disconnects == 1 && Button("AgentDisconnectButton").IsEnabled, "explicit disconnect finishes without changing enabled configuration");
        Check(Element<TextBlock>("AgentConnectionStatusLabel").Text.Contains("未连接") && _api.Connections == connections,
            "disconnected state is visible and editing never invokes refresh");
        _api.NextReconnectState = "auth-required"; NativeUi.Invoke(Button("AgentReconnectButton"));
        await WaitAsync(() => _api.Reconnects == 2 && Button("AgentReconnectButton").IsEnabled, "authentication-required reconnect finishes without an Errors array");
        Check(Element<InfoBar>("AgentStatusBar").Severity == InfoBarSeverity.Warning &&
            Element<TextBlock>("AgentConnectionStatusLabel").Text.Contains("MCP_AUTH_REQUIRED"),
            "HTTP success without a ready connection is never reported as connected");
        _api.ConnectionState = "auth-required"; _api.IncludeDiagnostics = true;
        NativeUi.Invoke(Button("AgentRefreshButton"));
        await WaitAsync(() => Button("AgentRefreshButton").IsEnabled && Element<TextBlock>("AgentConnectionStatusLabel").Text.Contains("MCP_AUTH_REQUIRED"),
            "authentication diagnostics expose a safe code without credentials");
        await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "http-zh.png"));
        var editor = Element<ScrollViewer>("AgentServerEditor");
        editor.ChangeView(null, editor.ScrollableHeight, null, disableAnimation: true); await SettleAsync();
        var saveBounds = Button("AgentSaveServerButton").TransformToVisual(editor).TransformBounds(new Windows.Foundation.Rect(0, 0,
            Button("AgentSaveServerButton").ActualWidth, Button("AgentSaveServerButton").ActualHeight));
        Check(editor.ScrollableHeight > 0 && Math.Abs(editor.VerticalOffset - editor.ScrollableHeight) < 2 &&
            saveBounds.Top >= 0 && saveBounds.Bottom <= editor.ActualHeight + 1, "scrolling the HTTP editor exposes its bottom save and delete controls");
        await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "http-bottom-zh.png"));
        tabs.SelectedIndex = 1; await SettleAsync();
        var skills = Element<ListView>("AgentSkillList"); skills.SelectedIndex = 0;
        await WaitAsync(() => Element<TextBox>("AgentSkillPreviewBox").Text.Contains("Example skill"), "diagnostic skill retains a readable unexecuted preview");
        Check(Element<TextBlock>("AgentSkillDiagnosticsLabel").Text.Contains("UNKNOWN_SKILL_FIELD") && Button("AgentImportSkillButton").IsEnabled,
            "skill diagnostics and explicit package import are available");
        int skillSaves = _api.Saves;
        Element<CheckBox>("AgentSkillEnabledBox").IsChecked = false;
        Check(_api.Saves == skillSaves && _window!.HasPendingChanges, "skill disable is a draft until explicitly saved");
        _api.ConflictNextSave = true; NativeUi.Invoke(Button("AgentSaveSkillButton"));
        await WaitAsync(() => _api.Saves == skillSaves + 1 && Button("AgentSaveSkillButton").IsEnabled, "skill conflict preserves its requested switch");
        Check(Element<CheckBox>("AgentSkillEnabledBox").IsChecked == false, "skill conflict does not automatically restore or overwrite the checkbox");
        NativeUi.Invoke(Button("AgentSaveSkillButton"));
        await WaitAsync(() => _api.Saves == skillSaves + 2 && !_window!.HasPendingChanges, "explicit skill save accepts the reloaded revision");
        Check(_api.Config.DisabledSkills!.Contains("skill-one"), "disabled skill identity is persisted in gateway configuration");
        Element<CheckBox>("AgentSkillEnabledBox").IsChecked = true; NativeUi.Invoke(Button("AgentSaveSkillButton"));
        await WaitAsync(() => _api.Saves == skillSaves + 3 && !_window!.HasPendingChanges, "skill can be explicitly enabled again");
        int previewReads = _api.PreviewReads; skills.SelectedIndex = 2; await SettleAsync();
        Check(_api.PreviewReads == previewReads && Element<TextBlock>("AgentSkillDiagnosticsLabel").Text.Contains("INVALID_APP_SKILL"),
            "unavailable skill displays its diagnosis without a failing preview request");
        await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "skill-diagnostics-zh.png"));
    }

    private void CheckRetryState()
    {
        var message = new ChatMessageState { Role = "assistant", Status = "error" };
        var presentation = new ConversationMessageViewModel(Guid.NewGuid(), message);
        presentation.SetRetryAllowed(true);
        Check(presentation.RetryVisibility == Visibility.Visible, "failed text-only replies retain the ordinary retry action");
        message.ToolActivities.Add(new("prior-tool", "file.write", null, "completed", "Already changed a fixture file"));
        presentation.Refresh();
        Check(presentation.RetryVisibility == Visibility.Collapsed, "failed replies with tool activity hide automatic retry");
        message.Status = "interrupted";
        presentation.Refresh();
        Check(presentation.RetryVisibility == Visibility.Collapsed, "interrupted replies with tool activity also hide retry");
        message.Status = "streaming";
        Check(!presentation.IsWaiting, "running tool activity replaces the empty waiting state");
        message.ToolActivities.Clear();
        message.Status = "interrupted";
        presentation.Refresh();
        Check(presentation.RetryVisibility == Visibility.Visible, "interrupted text-only replies keep their prior retry behavior");
    }
}
