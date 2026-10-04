using System.Runtime.InteropServices.WindowsRuntime;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Automation.Provider;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;

namespace WorkPaneUiSmoke;

public partial class App : Application
{
    private readonly string _directory = Path.Combine(Path.GetTempPath(), "kynxa-work-pane-" + Guid.NewGuid().ToString("N"));
    private readonly Guid _chatA = Guid.NewGuid(), _chatB = Guid.NewGuid();
    private readonly List<string> _checks = [];
    private readonly FakeWorkApi _api = new();
    private Window _window = null!;
    private Grid _layout = null!, _work = null!;
    private ConversationWorkTabs _tabs = null!;
    private ConversationScreenshotsPanel _screens = null!;
    private ConversationTerminalPanel _terminal = null!;
    private MountedWorkspaceHeader _mount = null!;
    private Guid? _chat;
    private bool _sidebarOpen = true;
    private string _png = "";
    private Exception? _unhandled;
    private string ResultPath => Path.Combine(_directory, "result.txt");

    public App()
    {
        Directory.CreateDirectory(_directory);
        Environment.SetEnvironmentVariable("KYNXA_DATA_HOME", Path.Combine(_directory, "Data"));
        Environment.SetEnvironmentVariable("KYNXA_EXTENSION_HOME", Path.Combine(_directory, "Extensions"));
        Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", "http://127.0.0.1:1");
        File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-work-pane-latest.txt"), ResultPath);
        InitializeComponent();
        UnhandledException += (_, args) => { _unhandled = args.Exception; args.Handled = true; };
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        File.WriteAllText(ResultPath, "RUNNING isolated tabbed work pane\n");
        UiText.Initialize("zh-CN");
        _tabs = new ConversationWorkTabs();
        _screens = new ConversationScreenshotsPanel(); _screens.SetTabbedMode(true); _screens.ConfigureApi(_api);
        _terminal = new ConversationTerminalPanel(); _terminal.SetTabbedMode(true); _terminal.ConfigureApi(_api);
        _mount = new MountedWorkspaceHeader(); _mount.ShowProject(Guid.NewGuid(), _directory);
        _work = new Grid { Margin = new Thickness(14, 36, 14, 14) };
        _work.RowDefinitions.Add(new() { Height = GridLength.Auto });
        _work.RowDefinitions.Add(new() { Height = GridLength.Auto });
        _work.RowDefinitions.Add(new() { Height = new GridLength(1, GridUnitType.Star) });
        _work.Children.Add(_mount); Grid.SetRow(_tabs, 1); _work.Children.Add(_tabs);
        Grid.SetRow(_screens, 2); Grid.SetRow(_terminal, 2); _work.Children.Add(_screens); _work.Children.Add(_terminal);
        _tabs.SelectedChanged += (_, _) => ApplyPane(); _tabs.TabsChanged += (_, _) => ApplyPane();
        _screens.ScreenshotsChanged += (_, _) => SyncTabs(); _terminal.TerminalChanged += (_, _) => SyncTabs();
        _layout = new Grid { Background = new SolidColorBrush(Microsoft.UI.Colors.White) };
        _layout.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        _layout.ColumnDefinitions.Add(new() { Width = new GridLength(350) });
        _layout.Children.Add(new TextBlock { Text = "独立验收窗口\n\n右侧为可切换标签的工作面板。\n新增截图和命令不会抢走正在查看的内容。",
            FontSize = 16, Margin = new Thickness(28, 48, 20, 20), TextWrapping = TextWrapping.Wrap });
        var divider = new Border { BorderThickness = new Thickness(1, 0, 0, 0), BorderBrush = (Brush)Resources["KynxaDividerBrush"] };
        Grid.SetColumn(divider, 1); _layout.Children.Add(divider); Grid.SetColumn(_work, 1); _layout.Children.Add(_work);
        _window = new Window { Title = "KYNXA · isolated tabbed work pane", Content = _layout };
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 600)); _window.Activate();
        _ = RunAsync();
    }

    private static string ShotKey(ConversationScreenshotSource source) => $"screenshot/{source.MessageId:D}/{source.Tool.ToolCallId}";
    private static string RunKey(TerminalOutputState.TerminalRun run) => $"terminal/{run.Identity.RequestId:D}/{run.Identity.ToolCallId}";
    private void Show(Guid chat, params ConversationMessageViewModel[] rows)
    {
        bool opened = _sidebarOpen;
        _sidebarOpen = false; ApplyPane(); _chat = chat;
        _screens.ShowConversation(chat, rows); _terminal.ShowConversation(chat, rows); SyncTabs();
        _sidebarOpen = opened; ApplyPane();
    }
    private void SyncTabs()
    {
        var items = _screens.Items.Select((source, index) => new ConversationWorkTab(ShotKey(source), "screenshot", UiText.Get("截图") + " " + (index + 1)))
            .Concat(_terminal.Items.Select(run => new ConversationWorkTab(RunKey(run), "terminal", run.Shell + " · " + run.Identity.ToolCallId))).ToArray();
        _tabs.ShowConversation(_chat, items); ApplyPane();
    }
    private void ApplyPane()
    {
        var selected = _tabs.SelectedTab;
        bool screenshot = _sidebarOpen && selected?.Kind == "screenshot";
        bool terminal = _sidebarOpen && selected?.Kind == "terminal";
        if (!screenshot) _screens.SetPreviewEnabled(false);
        if (!terminal) _terminal.SetPreviewEnabled(false);
        if (screenshot)
        {
            var source = _screens.Items.FirstOrDefault(item => ShotKey(item) == selected!.Key);
            if (source is not null) _screens.SelectSource(source.MessageId, source.Tool.ToolCallId);
            _screens.SetPreviewEnabled(source is not null);
        }
        if (terminal)
        {
            var run = _terminal.Items.FirstOrDefault(item => RunKey(item) == selected!.Key);
            if (run is not null) _terminal.SelectRun(run.Identity);
            _terminal.SetPreviewEnabled(run is not null);
        }
    }
    private T Element<T>(FrameworkElement parent, string name) where T : FrameworkElement => NativeUi.ByName<T>(parent, name);
    private Image Image => Element<Image>(_screens, "ScreenshotPanelImage");
    private TextBox Output => Element<TextBox>(_terminal, "TerminalPanelOutput");
    private Button TabButton(string key, bool close = false) => NativeUi.Descendants<Button>(_tabs)
        .Single(button => button.Name == (close ? "WorkTabClose" : "WorkTabSelect") && Equals(button.Tag, key));
    private void Check(bool condition, string label)
    { if (!condition) throw new InvalidOperationException(label); _checks.Add("PASS " + label); }
    private static Task SettleAsync() => Task.Delay(140);
    private async Task WaitAsync(Func<bool> condition, string label)
    {
        var until = DateTime.UtcNow.AddSeconds(8);
        while (!condition())
        {
            if (_unhandled is not null) throw new InvalidOperationException("Unhandled UI exception", _unhandled);
            if (DateTime.UtcNow > until) throw new TimeoutException(label + "; reads=" + _api.Reads.Count);
            await Task.Delay(20);
        }
    }
    private static ToolActivity Screenshot(string call, ToolResultReference? reference = null) =>
        new(call, "computer.screenshot", null, "completed", "Synthetic screenshot", ResultRef: reference ?? new(Guid.NewGuid(), 12000, new string('a', 64)));
    private ToolActivity Terminal(string call, string status = "running", ToolResultReference? reference = null) =>
        new(call, "terminal.host.run", JsonSerializer.SerializeToElement(new { shell = "powershell", script = "Write-Output 'synthetic'", cwd = _directory }),
            status, "Synthetic command", ResultRef: reference, WorkspaceRoot: _directory);
    private static ConversationMessageViewModel Row(Guid chat, string status, params ToolActivity[] tools) =>
        new(chat, new ChatMessageState { Role = "assistant", Content = "Synthetic final answer", Status = status, ToolActivities = tools.ToList() });
    private void CompleteImage(int index) => _api.Complete(index, new { content = new[] { new { type = "image", mimeType = "image/png", data = _png } } });

    private async Task RunAsync()
    {
        try
        {
            await WaitAsync(() => _work.XamlRoot is not null, "native work pane loaded"); _png = await EncodeAsync();
            var shotTool = Screenshot("shot-first"); var shotRow = Row(_chatA, "completed", shotTool);
            Show(_chatA, shotRow); await WaitAsync(() => _api.Reads.Count == 1, "first screenshot archive read");
            string shotKey = _tabs.SelectedTab!.Key;
            Check(_api.Reads[0].Chat == _chatA && _api.Reads[0].Reference == shotTool.ResultRef, "initial screenshot preserves exact chat and result identity");
            CompleteImage(0);
            await WaitAsync(() => NativeUi.Descendants<Image>(_screens).Any(image => image.Name == "ScreenshotPanelImage" && image.Source is not null), "initial preview decoded");
            Check(NativeUi.IsVisible(_screens) && !NativeUi.IsVisible(_terminal), "screenshot is the only visible content pane");
            Check(_mount.Visibility == Visibility.Visible && _mount.ActualHeight > 0 && _tabs.ActualHeight > 0,
                "mounted folder stays above the horizontal tab strip");
            var runningTool = Terminal("live-command"); var runningRow = Row(_chatA, "streaming", runningTool);
            Show(_chatA, shotRow, runningRow); _terminal.Append(_chatA, runningRow.Message.Id, new("live-command", 1, "stdout", "LIVE OUTPUT\n"));
            await SettleAsync(); string terminalKey = RunKey(_terminal.Items.Single());
            Check(_tabs.OpenItems.Count == 2 && _tabs.SelectedTab?.Key == shotKey && Image.Source is not null && !NativeUi.IsVisible(_terminal),
                "new live command opens in the background without replacing the screenshot");
            shotRow.Message.ToolActivities.Add(Screenshot("shot-second")); shotRow.Refresh(); await SettleAsync();
            string secondKey = ShotKey(_screens.Items.Single(item => item.Tool.ToolCallId == "shot-second"));
            Check(_tabs.OpenItems.Count == 3 && _tabs.SelectedTab?.Key == shotKey && _api.Reads.Count == 1,
                "new background screenshot is catalogued without downloading or taking focus");
            NativeUi.Invoke(TabButton(terminalKey)); await WaitAsync(() => Output.Text.Contains("LIVE OUTPUT") && NativeUi.IsVisible(_terminal), "native terminal tab selection");
            Check(!NativeUi.IsVisible(_screens) && NativeUi.IsVisible(_terminal) && _terminal.SelectedRun?.Identity.RequestId == runningRow.Message.Id,
                "tab click displays the exact command in the shared content cell");
            TerminalRunIdentity? stopped = null; _terminal.StopRequested += (_, identity) => stopped = identity;
            NativeUi.Invoke(TabButton(shotKey)); await WaitAsync(() => Image.Source is not null && NativeUi.IsVisible(_screens), "return to cached screenshot before closing background command");
            NativeUi.Invoke(TabButton(terminalKey, close: true)); await SettleAsync();
            _terminal.Append(_chatA, runningRow.Message.Id, new("live-command", 2, "stdout", "AFTER CLOSE\n")); await SettleAsync();
            Check(stopped is null && runningRow.Message.ToolActivities[0].Status == "running" && _terminal.Items.Single().Output.Contains("AFTER CLOSE") &&
                _tabs.OpenItems.All(item => item.Key != terminalKey), "closing a command tab neither stops the task nor loses continuing output");
            Check(_tabs.Reopen(terminalKey), "explicit reopen accepts closed command resource");
            await WaitAsync(() => Output.Text.Contains("AFTER CLOSE"), "reopened command shows retained text");
            NativeUi.Invoke(Element<Button>(_terminal, "TerminalPanelStop")); await SettleAsync();
            Check(stopped == new TerminalRunIdentity(_chatA, runningRow.Message.Id, "live-command"), "stop targets only the selected run's exact conversation/message/call");
            UiText.Initialize("en"); await SettleAsync();
            Check(NativeUi.IsVisible(_terminal) && !NativeUi.IsVisible(_screens) && Output.Text.Contains("AFTER CLOSE") && _api.Reads.Count == 1,
                "language changes preserve selection and cannot resurface both content panes");
            Check(AutomationProperties.GetName(Element<Button>(_tabs, "WorkTabsReopen")) == "Open tab", "tab reopen control updates its accessible language");
            UiText.Initialize("zh-CN");
            NativeUi.Invoke(TabButton(secondKey)); await WaitAsync(() => _api.Reads.Count == 2, "user selects new screenshot");
            Check(_api.Reads[1].Reference == shotRow.Message.ToolActivities[1].ResultRef, "selected screenshot uses its own immutable receipt reference");
            _sidebarOpen = false; ApplyPane(); CompleteImage(1); await SettleAsync();
            Check(_api.Reads[1].Token.IsCancellationRequested && Image.Source is null && !NativeUi.IsVisible(_screens) && !NativeUi.IsVisible(_terminal),
                "hiding sidebar cancels selected image download and hides both panes");
            for (int i = 0; i < 12; i++) { runningRow.Message.Content += "."; runningRow.Refresh(); SyncTabs(); }
            await SettleAsync(); Check(_api.Reads.Count == 2 && _tabs.SelectedTab?.Key == secondKey, "hidden streaming refresh cannot load archives or change active tab");
            _sidebarOpen = true; ApplyPane(); await WaitAsync(() => _api.Reads.Count == 3, "reopen retries cancelled selected screenshot");
            CompleteImage(2); await WaitAsync(() => Image.Source is not null, "reopened selected screenshot decoded");
            double narrowImage = Image.ActualWidth;
            _layout.ColumnDefinitions[1].Width = new GridLength(520); _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(1120, 720));
            await SettleAsync();
            Check(Image.ActualWidth > narrowImage && Image.ActualWidth <= _screens.ActualWidth && _api.Reads.Count == 3,
                "screenshot grows with sidebar width using retained archive bytes");
            ToolResultRequest? opened = null; _screens.ScreenshotOpenRequested += (_, request) => opened = request;
            NativeUi.Invoke(Element<Button>(_screens, "ScreenshotPanelOpen")); await SettleAsync();
            Check(opened?.ConversationId == _chatA && opened.MessageId == shotRow.Message.Id && opened.Tool.ToolCallId == "shot-second" &&
                opened.Tool.ResultRef == shotRow.Message.ToolActivities[1].ResultRef,
                "fullscreen request preserves exact selected screenshot and formal reference");
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "work-tabs-wide.png"));

            var historicalTool = Terminal("archive-command", "completed", new(Guid.NewGuid(), 100, new string('b', 64)));
            var historicalRow = Row(_chatB, "completed", historicalTool);
            Show(_chatB, historicalRow); await WaitAsync(() => _api.Reads.Count == 4, "selected historical terminal starts archive load");
            var archive = _api.Reads[3];
            for (int i = 0; i < 15; i++) { _terminal.SelectRun(_terminal.SelectedRun!.Identity); SyncTabs(); }
            await SettleAsync();
            Check(_api.Reads.Count == 4 && !archive.Token.IsCancellationRequested,
                "unchanged selected command and repeated layout refresh keep one archive request alive");
            Show(_chatA, shotRow, runningRow); _api.Complete(3, new { structuredContent = new { boundary = "host-terminal", stdout = "WRONG CHAT", stderr = "", exitCode = 0 } });
            await WaitAsync(() => _api.Reads.Count == 5, "returning chat reloads its selected screenshot archive");
            CompleteImage(4); await WaitAsync(() => Image.Source is not null, "returned chat selected image decoded");
            await SettleAsync();
            Check(_tabs.SelectedTab?.Key == secondKey && !NativeUi.IsVisible(_terminal) && !Output.Text.Contains("WRONG CHAT"),
                "returning to selected history rejects a late terminal archive from another conversation");
            for (int i = 0; i < 15; i++) shotRow.Message.ToolActivities.Add(Screenshot("background-wide-" + i));
            shotRow.Refresh(); await SettleAsync();
            int reads = _api.Reads.Count;
            _layout.ColumnDefinitions[1].Width = new GridLength(230); _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(740, 500)); await SettleAsync();
            var scroll = Element<ScrollViewer>(_tabs, "WorkTabsScroll");
            Check(scroll.ScrollableWidth > 0 && scroll.ActualWidth <= _tabs.ActualWidth && _tabs.ActualWidth <= _work.ActualWidth,
                "many tabs scroll horizontally within the narrowed sidebar");
            Check(_tabs.SelectedTab?.Key == secondKey && _api.Reads.Count == reads && NativeUi.IsVisible(_screens) && !NativeUi.IsVisible(_terminal),
                "adding many tabs and resizing preserve selected content without background image fetches");
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "work-tabs-narrow.png"));
            foreach (var background in _tabs.OpenItems.Where(item => item.Key != _tabs.SelectedTab?.Key).ToArray()) _tabs.Close(background.Key);
            _tabs.CloseSelected(); await SettleAsync();
            Check(_tabs.HasItems && !_tabs.HasOpenTabs && !NativeUi.IsVisible(_screens) && !NativeUi.IsVisible(_terminal) && stopped?.ToolCallId == "live-command",
                "closing every tab keeps resources reopenable and leaves mounted-folder header intact");
            shotRow.Message.ToolActivities[0] = shotTool with { ResultRef = new(Guid.NewGuid(), 14000, new string('c', 64)) };
            shotRow.Refresh(); await SettleAsync();
            Check(_tabs.Items.Any(item => item.Key == shotKey) && !_tabs.HasOpenTabs && _api.Reads.Count == reads,
                "a closed screenshot's new archive reference cannot reopen the logical tab or trigger a hidden download");
            Check(_tabs.Reopen(secondKey), "closed screenshot can be explicitly reopened from resource catalog");
            await WaitAsync(() => Image.Source is not null, "reopened screenshot retained cache");
            Check(_tabs.SelectedTab?.Key == secondKey && _api.Reads.Count == reads, "reopen selects the requested history image without taking another snapshot");
            await CheckHeaderLifecycleAsync();
            Check(_unhandled is null, "tabbed pane lifecycle has no unhandled native UI exception");
            File.WriteAllText(ResultPath, string.Join('\n', _checks) + $"\nPASS: {_checks.Count} tabbed work pane checks.\nPreviews: {_directory}\n");
        }
        catch (Exception error)
        {
            File.WriteAllText(ResultPath, string.Join('\n', _checks) + "\nFAIL: " + error + "\n"); Environment.ExitCode = 1;
        }
        finally { _tabs.Dispose(); _screens.Dispose(); _terminal.Dispose(); _mount.Dispose(); _window.Close(); Exit(); }
    }

    private async Task CheckHeaderLifecycleAsync()
    {
        var headers = Enumerable.Range(0, 16).Select(index =>
            new ConversationWorkTab("shared-key-" + index, "terminal", "Long synthetic terminal resource " + index)).ToArray();
        _tabs.ShowConversation(_chatA, headers); await SettleAsync();
        var scroll = Element<ScrollViewer>(_tabs, "WorkTabsScroll");
        scroll.ChangeView(190, null, null, true);
        await WaitAsync(() => Math.Abs(scroll.HorizontalOffset - 190) < 2, "horizontal tab strip can scroll independently of content");
        var added = headers.Concat([new ConversationWorkTab("background-new", "screenshot", "New synthetic background screenshot")]).ToArray();
        _tabs.ShowConversation(_chatA, added); await SettleAsync();
        Check(Math.Abs(scroll.HorizontalOffset - 190) < 2 && _tabs.SelectedTab?.Key == headers[0].Key,
            "background tab insertion preserves both horizontal reading offset and selected key");
        added[0] = added[0] with { Title = "Updated synthetic terminal completion status" };
        _tabs.ShowConversation(_chatA, added); await SettleAsync();
        Check(Math.Abs(scroll.HorizontalOffset - 190) < 2,
            "mutable title/status refresh preserves the horizontally scrolled tab strip");
        UiText.Initialize("en"); await SettleAsync();
        Check(Math.Abs(scroll.HorizontalOffset - 190) < 2 && _tabs.SelectedTab?.Key == headers[0].Key,
            "localization rebuild preserves tab selection and horizontal reading offset");
        int selectedEvents = 0;
        _tabs.SelectedChanged += (_, _) => selectedEvents++;
        NativeUi.Invoke(Element<Button>(_tabs, "WorkTabsReopen"));
        MenuFlyoutItem? stale = null;
        await WaitAsync(() =>
        {
            stale = VisualTreeHelper.GetOpenPopupsForXamlRoot(_tabs.XamlRoot)
                .Where(popup => popup.Child is not null).SelectMany(popup => NativeUi.Descendants<MenuFlyoutItem>(popup.Child))
                .FirstOrDefault(item => Equals(item.Tag, headers[2].Key));
            return stale is not null;
        }, "resource reopen menu exposes native selectable entries");
        _tabs.ShowConversation(_chatB, added); await SettleAsync();
        Check(selectedEvents == 1 && _tabs.SelectedTab?.Key == headers[0].Key,
            "a chat change emits exact selection notification even when logical tab keys match");
        var peer = FrameworkElementAutomationPeer.CreatePeerForElement(stale!);
        if (peer?.GetPattern(PatternInterface.Invoke) is not IInvokeProvider invoke)
            throw new InvalidOperationException("Reopen menu entry has no native Invoke pattern.");
        invoke.Invoke(); await SettleAsync();
        Check(_tabs.SelectedTab?.Key == headers[0].Key && selectedEvents == 1,
            "a reopen menu retained from another chat cannot change the current chat's selected resource");
    }

    private static async Task<string> EncodeAsync()
    {
        const uint width = 900, height = 450;
        byte[] pixels = new byte[width * height * 4];
        for (uint y = 0; y < height; y++) for (uint x = 0; x < width; x++)
        {
            int offset = checked((int)((y * width + x) * 4));
            pixels[offset] = y < 50 ? (byte)210 : (byte)247;
            pixels[offset + 1] = x < 160 ? (byte)222 : (byte)247;
            pixels[offset + 2] = (byte)(y % 40 < 6 ? 190 : 247); pixels[offset + 3] = 255;
        }
        using var stream = new InMemoryRandomAccessStream();
        var encoder = await BitmapEncoder.CreateAsync(BitmapEncoder.PngEncoderId, stream);
        encoder.SetPixelData(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Ignore, width, height, 96, 96, pixels);
        await encoder.FlushAsync(); stream.Seek(0);
        using var output = new MemoryStream(); await stream.AsStreamForRead().CopyToAsync(output);
        return Convert.ToBase64String(output.ToArray());
    }
}

internal sealed record WorkRead(Guid Chat, ToolResultReference Reference, CancellationToken Token, TaskCompletionSource<ToolResultResponse> Completion);
internal sealed class FakeWorkApi : IAgentApi
{
    public List<WorkRead> Reads { get; } = [];
    public Task<ToolResultResponse> GetToolResultAsync(Guid conversationId, ToolResultReference reference, CancellationToken cancellationToken = default)
    {
        var completion = new TaskCompletionSource<ToolResultResponse>(TaskCreationOptions.RunContinuationsAsynchronously);
        Reads.Add(new(conversationId, reference, cancellationToken, completion));
        // Ignoring cancellation deliberately exercises each production pane's late-result guard.
        return completion.Task;
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
