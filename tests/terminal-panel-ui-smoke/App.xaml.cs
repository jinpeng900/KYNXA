using System.Text;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace TerminalPanelUiSmoke;

public partial class App : Application
{
    private readonly string _directory = Path.Combine(Path.GetTempPath(), "kynxa-terminal-panel-" + Guid.NewGuid().ToString("N"));
    private readonly List<string> _checks = [];
    private readonly FakeTerminalApi _api = new();
    private readonly Guid _chatA = Guid.NewGuid(), _chatB = Guid.NewGuid(), _request = Guid.NewGuid();
    private Window _window = null!;
    private ConversationTerminalPanel _panel = null!;
    private Grid _layout = null!;
    private bool _demo;

    public App()
    {
        Directory.CreateDirectory(_directory);
        Environment.SetEnvironmentVariable("KYNXA_DATA_HOME", Path.Combine(_directory, "Data"));
        Environment.SetEnvironmentVariable("KYNXA_EXTENSION_HOME", Path.Combine(_directory, "Extensions"));
        InitializeComponent();
        UnhandledException += (_, args) => { File.AppendAllText(Path.Combine(_directory, "result.txt"), "UNHANDLED " + args.Exception); args.Handled = true; };
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        _demo = Environment.GetCommandLineArgs().Contains("--live-demo");
        File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-terminal-panel-latest.txt"), Path.Combine(_directory, "result.txt"));
        UiText.Initialize("zh-CN");
        _panel = new ConversationTerminalPanel { Margin = new Thickness(14, 46, 14, 14) };
        _panel.ConfigureApi(_api);
        _layout = new Grid { Background = new SolidColorBrush(Microsoft.UI.Colors.White) };
        _layout.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        _layout.ColumnDefinitions.Add(new() { Width = new GridLength(350) });
        _layout.Children.Add(new TextBlock { Text = "KYNXA\n\n终端位于右侧栏\n可选择复制输出，关闭面板不取消命令。", FontSize = 16, Margin = new Thickness(30, 50, 20, 20), TextWrapping = TextWrapping.Wrap });
        var divider = new Border { BorderThickness = new Thickness(1, 0, 0, 0), BorderBrush = (Brush)Resources["KynxaDividerBrush"] };
        Grid.SetColumn(divider, 1); _layout.Children.Add(divider);
        Grid.SetColumn(_panel, 1); _layout.Children.Add(_panel);
        _window = new Window { Title = "KYNXA · isolated terminal panel", Content = _layout };
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(940, 570));
        _window.Activate();
        _ = _demo ? RunDemoAsync() : RunAsync();
    }

    private T Element<T>(string name) where T : FrameworkElement => NativeUi.ByName<T>(_panel, name);
    private TextBox Output => Element<TextBox>("TerminalPanelOutput");
    private TextBlock Status => Element<TextBlock>("TerminalPanelStatus");
    private void Check(bool condition, string label) { if (!condition) throw new InvalidOperationException(label); _checks.Add("PASS " + label); }
    private static Task SettleAsync() => Task.Delay(140);
    private async Task WaitAsync(Func<bool> condition, string label)
    {
        var until = DateTime.UtcNow.AddSeconds(5);
        while (!condition()) { if (DateTime.UtcNow > until) throw new TimeoutException(label); await Task.Delay(20); }
    }

    private ToolActivity Tool(string call, string status = "running", string? result = null, ToolResultReference? reference = null) =>
        new(call, "terminal.host.run", JsonSerializer.SerializeToElement(new { shell = "powershell", script = "Write-Output 'KYNXA 示例输出'", cwd = _directory }), status,
            "", Result: result, WorkspaceRoot: _directory, ResultRef: reference);

    private ConversationMessageViewModel Row(Guid chat, Guid request, params ToolActivity[] tools) =>
        new(chat, new ChatMessageState { Id = request, Role = "assistant", Status = "streaming", ToolActivities = tools.ToList() });

    private void CheckState()
    {
        var state = new TerminalOutputState();
        var identity = new TerminalRunIdentity(_chatA, _request, "run-one");
        Check(state.Observe(identity, Tool("run-one"), true), "new command has exact chat/request/call ownership");
        Check(!state.Append(_chatB, _request, new("run-one", 1, "stdout", "wrong")) &&
            !state.Append(_chatA, Guid.NewGuid(), new("run-one", 1, "stdout", "wrong")), "foreign chat/request output rejected");
        Check(!state.Append(_chatA, _request, new("other-call", 1, "stdout", "wrong")), "foreign tool call output rejected");
        Check(state.Append(_chatA, _request, new("run-one", 1, "stdout", "正确输出\n")), "UTF8 public stream accepted");
        Check(!state.Append(_chatA, _request, new("run-one", 1, "stdout", "duplicate")) &&
            !state.Append(_chatA, _request, new("run-one", 3, "stdout", "gap")), "duplicate and out-of-order output rejected");
        state.Close(_chatA); state.Append(_chatA, _request, new("run-one", 2, "stderr", "可读错误\n"));
        Check(state.IsClosed(_chatA) && state.Selected(_chatA)!.Output.Contains("可读错误"), "close preserves execution and incoming output without reopening");
        for (int sequence = 3; sequence <= 10; sequence++) state.Append(_chatA, _request, new("run-one", sequence, "stdout", new string('数', 30000)));
        Check(Encoding.UTF8.GetByteCount(state.Selected(_chatA)!.Output) <= TerminalOutputState.MaximumOutputBytes &&
            !state.Selected(_chatA)!.Output.Contains('\uFFFD') && state.Selected(_chatA)!.Truncated, "large multilingual output has bounded valid UTF8 tail");
        Check(state.Append(_chatA, _request, new("run-one", 11, "console", "screen snapshot", true)) && state.Selected(_chatA)!.Output == "screen snapshot", "console snapshots replace rather than append");
        state.EndRequest(_chatA, _request);
        Check(state.Selected(_chatA)!.Status == "unknown", "lost completion receipt never becomes success");
        var second = new TerminalRunIdentity(_chatA, _request, "run-two");
        state.Observe(second, Tool("run-two", "error", JsonSerializer.Serialize(new { stdout = "真实输出", stderr = "失败信息", exitCode = 7 })), true);
        Check(!state.IsClosed(_chatA) && state.Selected(_chatA)!.ExitCode == 7 && state.Selected(_chatA)!.Status == "error", "new command may reopen and nonzero exit remains failure");
        Check(state.Selected(_chatA)!.Output == "真实输出\n失败信息", "known receipt fallback displays text without JSON");
        for (int i = 0; i < 20; i++) state.Observe(new(_chatA, _request, "recent-" + i), Tool("recent-" + i), true);
        Check(state.Runs(_chatA).Count == 8, "per-chat recent run cache stays bounded to eight");
        state.Observe(new(_chatB, _request, "old-running"), Tool("old-running"), false, requestActive: false);
        Check(state.Selected(_chatB)!.Status == "unknown", "historical unfinished command is not presented as executing");
        for (int i = 0; i < 20; i++) state.Observe(new(Guid.NewGuid(), _request, "chat-" + i), Tool("chat-" + i), false);
        Check(state.Runs(_chatA).Count == 0, "old conversation display caches are evicted");
    }

    private async Task RunAsync()
    {
        try
        {
            CheckState();
            _panel.ShowConversation(_chatA, []); _panel.SetPreviewEnabled(true); await SettleAsync();
            Check(!_panel.HasTerminal && _panel.Visibility == Visibility.Collapsed, "empty chat has no terminal module");
            var tool = Tool("live-call");
            var row = Row(_chatA, _request, tool);
            _panel.Observe(_chatA, _request, tool); _panel.ShowConversation(_chatA, [row]);
            _panel.Append(_chatA, _request, new("live-call", 1, "stdout", "开始执行\n第一行输出\n"));
            await WaitAsync(() => Output.Text.Contains("第一行输出"), "live output visible");
            Check(_panel.Visibility == Visibility.Visible && Element<TextBox>("TerminalPanelCommand").Text.Contains("Write-Output"), "command and live text shown in native sidebar");
            Check(Output.IsReadOnly && Output.FontSize == 14 && Element<TextBlock>("TerminalPanelTitle").FontSize == 12, "selectable normal-size monospaced output and compact grey title");
            TerminalRunIdentity? stopped = null; _panel.StopRequested += (_, identity) => stopped = identity;
            NativeUi.Invoke(Element<Button>("TerminalPanelStop")); await SettleAsync();
            Check(stopped == new TerminalRunIdentity(_chatA, _request, "live-call"), "stop event preserves exact chat/request/tool identity");
            Output.Select(0, 4); string selected = Output.SelectedText;
            _panel.Append(_chatA, _request, new("live-call", 2, "stdout", "第二行输出\n")); await SettleAsync();
            Check(Output.SelectedText == selected && !Output.Text.Contains("第二行"), "selected terminal text freezes until selection clears");
            Output.Select(0, 0); await WaitAsync(() => Output.Text.Contains("第二行"), "clear selection applies latest output");
            Check(Output.SelectedText.Length == 0, "clearing terminal selection releases the buffered update");
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "terminal-active.png"));
            NativeUi.Invoke(Element<Button>("TerminalPanelClose")); await SettleAsync();
            Check(_panel.HasTerminal && _panel.Visibility == Visibility.Collapsed, "close hides module without deleting command");
            _panel.Append(_chatA, _request, new("live-call", 3, "stdout", "关闭后仍接收\n")); await SettleAsync();
            Check(_panel.Visibility == Visibility.Collapsed, "incoming chunks do not reopen closed panel");
            _panel.ReopenSelected(); await WaitAsync(() => Output.Text.Contains("关闭后仍接收"), "explicit reopen recovers hidden output");
            Check(_panel.Visibility == Visibility.Visible, "explicit reopen shows retained run");
            _panel.ShowConversation(_chatB, []); await SettleAsync();
            _panel.Append(_chatA, _request, new("live-call", 4, "stdout", "另一聊天后台输出\n")); await SettleAsync();
            Check(!_panel.HasTerminal && Output.Text.Length == 0, "switching chats cannot leak another run's output");
            _panel.ShowConversation(_chatA, [row]); await WaitAsync(() => Output.Text.Contains("另一聊天后台输出"), "return to chat restores its output");
            Check(_panel.HasTerminal, "returning to original chat selects its retained run");
            UiText.Initialize("en"); await WaitAsync(() => Element<TextBlock>("TerminalPanelTitle").Text.StartsWith("Terminal"), "language hot switch");
            Check(AutomationProperties.GetName(Element<Button>("TerminalPanelStop")) == "Stop execution" && Output.Text.Contains("开始执行"), "language updates controls but preserves command output");
            var complete = Tool("live-call", "completed", JsonSerializer.Serialize(new { stdout = "receipt fallback must not overwrite live stream", stderr = "", exitCode = 0 }));
            row.Message.ToolActivities[0] = complete; row.Message.Status = "completed"; row.Refresh();
            _panel.Observe(_chatA, _request, complete); await WaitAsync(() => Status.Text.Contains("Exit code 0"), "completion receipt visible");
            Check(!Element<Button>("TerminalPanelStop").IsEnabled && Output.Text.Contains("后台输出"), "completion disables stop and keeps live text");
            _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(520, 420)); _layout.ColumnDefinitions[1].Width = new GridLength(270); await SettleAsync();
            Check(Output.ActualWidth <= _panel.ActualWidth && _panel.ActualWidth <= 270, "narrow sidebar wraps within available width");
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "terminal-narrow.png"));
            UiText.Initialize("zh-CN");
            var reference = new ToolResultReference(Guid.NewGuid(), 100, new string('a', 64));
            var archived = Tool("archive-call", "completed", "{\"kind\":\"result_preview\",\"preview\":\"truncated JSON\"}", reference);
            var archivedRow = Row(_chatB, Guid.NewGuid(), archived); archivedRow.Message.Status = "completed";
            _panel.ShowConversation(_chatB, [archivedRow]);
            await WaitAsync(() => _api.Reads.Count == 1, "historical archive lazily requested");
            Check(_api.Reads[0].Chat == _chatB && _api.Reads[0].Reference == reference, "archive loader keeps exact chat/result identity");
            _api.Complete(0, new { content = Array.Empty<object>(), structuredContent = new { boundary = "host-terminal", stdout = "历史真实输出\n", stderr = "历史错误", exitCode = 5 } });
            await WaitAsync(() => Output.Text.Contains("历史真实输出"), "canonical terminal receipt recovered");
            Check(Output.Text.Contains("历史错误") && !Output.Text.Contains("structuredContent") && Status.Text.Contains("5"), "history uses canonical plain output without JSON");
            int reads = _api.Reads.Count;
            for (int i = 0; i < 10; i++) { archivedRow.Message.Content += "."; archivedRow.Refresh(); _panel.ShowConversation(_chatB, [archivedRow]); }
            await SettleAsync(); Check(_api.Reads.Count == reads, "unrelated chat streaming never rereads output archive");
            var late = new ToolResultReference(Guid.NewGuid(), 100, new string('b', 64));
            var lateRow = Row(_chatA, Guid.NewGuid(), Tool("late-archive", "completed", null, late)); lateRow.Message.Status = "completed";
            _panel.ShowConversation(_chatA, [lateRow]); await WaitAsync(() => _api.Reads.Count == 2, "late archive read begins");
            _panel.ShowConversation(_chatB, [archivedRow]);
            _api.Complete(1, new { structuredContent = new { boundary = "host-terminal", stdout = "WRONG CHAT LATE OUTPUT", stderr = "", exitCode = 0 } });
            await SettleAsync(); Check(!Output.Text.Contains("WRONG CHAT") && Output.Text.Contains("历史真实输出"), "late cancelled archive cannot overwrite selected chat");
            _panel.EndRequest(_chatB, archivedRow.Message.Id); await SettleAsync();
            Check(Status.Text.StartsWith("执行完成"), "ending request does not overwrite an actual completion receipt");
            _panel.Dispose(); _panel.Append(_chatB, archivedRow.Message.Id, new("archive-call", 1, "stdout", "disposed"));
            Check(!Output.Text.Contains("disposed"), "disposed module stops observers and output callbacks");
            File.WriteAllText(Path.Combine(_directory, "result.txt"), $"PASS: {_checks.Count} terminal panel checks.\n" + string.Join('\n', _checks));
            _window.Close(); Exit();
        }
        catch (Exception error)
        {
            File.WriteAllText(Path.Combine(_directory, "result.txt"), "FAIL " + error + "\n" + string.Join('\n', _checks));
            _panel.Dispose(); _window.Close(); Exit();
        }
    }
}

internal sealed class FakeTerminalApi : IAgentApi
{
    public List<(Guid Chat, ToolResultReference Reference, TaskCompletionSource<ToolResultResponse> Completion)> Reads { get; } = [];
    public Task<ToolResultResponse> GetToolResultAsync(Guid chat, ToolResultReference reference, CancellationToken cancellationToken = default)
    {
        var completion = new TaskCompletionSource<ToolResultResponse>(TaskCreationOptions.RunContinuationsAsynchronously);
        Reads.Add((chat, reference, completion)); return completion.Task;
    }
    public void Complete(int index, object receipt) => Reads[index].Completion.TrySetResult(new(JsonSerializer.SerializeToElement(receipt)));
    public Task<AgentConfig> GetConfigAsync(CancellationToken cancellationToken = default) => throw new NotSupportedException();
    public Task<AgentConfig> SaveConfigAsync(AgentConfigSaveRequest request, CancellationToken cancellationToken = default) => throw new NotSupportedException();
    public Task<AgentSkill[]> GetSkillsAsync(Guid? conversationId = null, CancellationToken cancellationToken = default) => throw new NotSupportedException();
    public Task<AgentSkillDetail> GetSkillAsync(string id, Guid? conversationId = null, CancellationToken cancellationToken = default) => throw new NotSupportedException();
    public Task<AgentToolsResponse> GetToolsAsync(CancellationToken cancellationToken = default) => throw new NotSupportedException();
    public Task<AgentToolsResponse> RefreshMcpAsync(Guid? conversationId = null, CancellationToken cancellationToken = default) => throw new NotSupportedException();
    public Task<AgentApprovalResponse> SubmitApprovalAsync(AgentApprovalRequest request, CancellationToken cancellationToken = default) => throw new NotSupportedException();
}
