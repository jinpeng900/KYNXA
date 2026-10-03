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

    private T Element<T>(string name) where T : FrameworkElement => NativeUi.ByName<T>(_root, name);
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
            Check(Element<PivotItem>("AgentServersTab").Header?.ToString() == "MCP 服务" && Element<PivotItem>("AgentSkillsTab").Header?.ToString() == "技能", "Chinese management tabs render");
            var list = Element<ListView>("AgentServerList");
            list.SelectedItem = list.Items[0];
            await SettleAsync();
            var command = Element<TextBox>("AgentServerCommandBox");
            Check(command.Text == "example-mcp" && Element<TextBox>("AgentServerIdBox").IsReadOnly, "native server selection fills identity and command");
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
            var skills = Element<ListView>("AgentSkillList");
            skills.SelectedItem = skills.Items[0];
            await WaitAsync(() => Element<TextBox>("AgentSkillPreviewBox").Text.Replace("\r\n", "\n").Replace('\r', '\n') == _api.PreviewText, "native skill selection reads its preview");
            Check(Element<TextBox>("AgentSkillPreviewBox").IsReadOnly && _api.Connections == 0 && _api.Saves == 3, "skill script text remains an unexecuted read-only preview");
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
            var directories = Element<ListView>("AgentDirectoryList");
            directories.SelectedItem = directories.Items[0];
            NativeUi.Invoke(Button("AgentRemoveDirectoryButton"));
            Check(_api.Config.SkillDirectories.Length == 1 && _window.HasPendingChanges, "directory edits remain unsaved in memory");
            NativeUi.Invoke(Button("AgentSaveDirectoriesButton"));
            await WaitAsync(() => _api.Saves == 4 && _api.Config.SkillDirectories.Length == 0 && !_window.HasPendingChanges, "explicit directory save updates the gateway configuration");
            tabs.SelectedIndex = 2;
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
            tabs.SelectedIndex = 1;
            await SettleAsync();
            var closingPreview = new TaskCompletionSource<AgentSkillDetail>(TaskCreationOptions.RunContinuationsAsynchronously);
            _api.DelayedPreview = closingPreview;
            skills.SelectedItem = skills.Items[1];
            await WaitAsync(() => _api.PreviewReads == 4, "closing fixture has a pending preview");
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
        UiText.Initialize("en");
        await SettleAsync();
        Check(dialog.PrimaryButtonText == "Approve once" && dialog.CloseButtonText == "Deny" &&
            NativeUi.ByName<TextBlock>(dialog, "ToolApprovalScope").Text.Contains("outside"), "open approval labels change language immediately");
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
