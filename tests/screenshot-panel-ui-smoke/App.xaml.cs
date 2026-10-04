using System.Collections;
using System.Reflection;
using System.Runtime.InteropServices.WindowsRuntime;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;

namespace ScreenshotPanelUiSmoke;

public partial class App : Application
{
    private readonly string _directory = Path.Combine(Path.GetTempPath(), "kynxa-screenshot-panel-" + Guid.NewGuid().ToString("N"));
    private readonly List<string> _checks = [];
    private readonly FakeScreenshotApi _api = new();
    private readonly Guid _chatA = Guid.NewGuid(), _chatB = Guid.NewGuid();
    private Window _window = null!;
    private ConversationScreenshotsPanel _panel = null!;
    private Grid _layout = null!;
    private string _png = "";
    private Exception? _unhandled;
    private string ResultPath => Path.Combine(_directory, "result.txt");

    public App()
    {
        Directory.CreateDirectory(_directory);
        Environment.SetEnvironmentVariable("KYNXA_DATA_HOME", Path.Combine(_directory, "Data"));
        Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", "http://127.0.0.1:1");
        File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-screenshot-panel-latest.txt"), ResultPath);
        InitializeComponent();
        UnhandledException += (_, args) => { _unhandled = args.Exception; args.Handled = true; };
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        File.WriteAllText(ResultPath, "RUNNING: isolated production screenshot panel\n");
        UiText.Initialize("zh-CN");
        _panel = new ConversationScreenshotsPanel { Margin = new Thickness(14, 42, 14, 14) };
        _panel.ConfigureApi(_api);
        _layout = new Grid { Background = new SolidColorBrush(Microsoft.UI.Colors.White) };
        _layout.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        _layout.ColumnDefinitions.Add(new() { Width = new GridLength(290) });
        _layout.Children.Add(new TextBlock { Text = "隔离界面验收\n\n聊天正文保持为文本，截图位于右侧模块。", FontSize = 16,
            Margin = new Thickness(30, 42, 20, 20), TextWrapping = TextWrapping.Wrap });
        var divider = new Border { BorderThickness = new Thickness(1, 0, 0, 0), BorderBrush = (Brush)Resources["KynxaDividerBrush"] };
        Grid.SetColumn(divider, 1); _layout.Children.Add(divider);
        Grid.SetColumn(_panel, 1); _layout.Children.Add(_panel);
        _window = new Window { Title = "KYNXA screenshot panel · isolated fixture", Content = _layout };
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(880, 500));
        _window.Activate();
        _ = RunAsync();
    }

    private T Element<T>(string name) where T : FrameworkElement => NativeUi.ByName<T>(_panel, name);
    private Image Image => Element<Image>("ScreenshotPanelImage");
    private Button Button(string name) => Element<Button>(name);
    private void Check(bool condition, string label)
    { if (!condition) throw new InvalidOperationException(label); _checks.Add("PASS " + label); }
    private async Task WaitAsync(Func<bool> condition, string label)
    {
        var until = DateTime.UtcNow.AddSeconds(10);
        while (!condition())
        {
            if (_unhandled is not null) throw new InvalidOperationException("Unhandled fixture UI exception", _unhandled);
            if (DateTime.UtcNow > until) throw new TimeoutException(label + $"; reads={_api.Reads.Count}");
            await Task.Delay(20);
        }
        Check(true, label);
    }
    private static Task SettleAsync() => Task.Delay(100);
    private static ToolActivity Screenshot(string call, string status = "completed", ToolResultReference? reference = null) =>
        new(call, "computer.screenshot", null, status, "Screenshot", ResultRef: reference ?? new(Guid.NewGuid(), 12000, new string('a', 64)));
    private static ConversationMessageViewModel Message(Guid chat, params ToolActivity[] tools) =>
        new(chat, new ChatMessageState { Role = "assistant", Content = "已读取页面并完成回复。", ToolActivities = tools.ToList() });
    private void Complete(int index, string? encoded = null) => _api.Reads[index].Completion.TrySetResult(new(JsonSerializer.SerializeToElement(
        new { content = new[] { new { type = "image", mimeType = "image/png", data = encoded ?? _png } } })));

    private async Task RunAsync()
    {
        try
        {
            await WaitAsync(() => _layout.XamlRoot is not null, "production panel loads in an isolated native window");
            _png = await EncodeAsync(BitmapEncoder.PngEncoderId, 600, 300);
            CheckSources();
            await CheckDecoderAsync();
            await CheckMountedWorkspaceAsync();
            await CheckMountedScreenshotLayoutAsync();
            await CheckResponsiveScreenshotAsync();
            await CheckShellScreenshotRoutingAsync();
            await CheckScreenshotViewerAsync();
            await CheckBrowserScreenshotsAsync();
            var first = Screenshot("shot-a-1"); var second = Screenshot("shot-a-2");
            var row = Message(_chatA, first, second);
            int changes = 0; _panel.ScreenshotsChanged += (_, _) => changes++;
            _panel.ShowConversation(_chatA, [row]);
            await WaitAsync(() => _panel.IsLoaded, "visible screenshot module completes native loading");
            Check(_panel.HasScreenshots && _api.Reads.Count == 0 && _panel.Visibility == Visibility.Visible, "collapsed preview discovers receipts without downloading images");
            EventHandler openSidebar = (_, _) => _panel.SetPreviewEnabled(_panel.HasScreenshots);
            _panel.ScreenshotsChanged += openSidebar;
            _panel.ShowConversation(null, []);
            _panel.ShowConversation(_chatA, [row]);
            await WaitAsync(() => _api.Reads.Count == 1, "opening preview reads only the latest screenshot");
            await SettleAsync();
            Check(_api.Reads.Count == 1 && !_api.Reads[0].Token.IsCancellationRequested, "synchronous sidebar opening during receipt notification does not cancel and duplicate the archive request");
            _panel.ScreenshotsChanged -= openSidebar;
            Check(_api.Reads[0].Conversation == _chatA && _api.Reads[0].Reference == second.ResultRef, "archive request carries the current chat and formal result reference");
            Complete(0); await WaitAsync(() => Image.Source is not null, "typed PNG archive becomes a thumbnail");
            Check(Image.ActualWidth <= _panel.ActualWidth && Image.ActualHeight <= _panel.ActualHeight, "thumbnail fits the available sidebar width and height");
            var original = Image.Source;
            int stableChanges = changes;
            for (int i = 0; i < 30; i++) { row.Message.Content += "·"; row.Refresh(); _panel.ShowConversation(_chatA, [row]); }
            await SettleAsync();
            Check(_api.Reads.Count == 1 && ReferenceEquals(original, Image.Source) && changes == stableChanges, "streaming text notifications do not reload thumbnails or change gallery identity");
            ToolResultRequest? opened = null; _panel.ScreenshotOpenRequested += (_, request) => opened = request;
            NativeUi.Invoke(Button("ScreenshotPanelOpen")); await SettleAsync();
            Check(opened?.ConversationId == _chatA && opened.MessageId == row.Message.Id && opened.Tool == second, "thumbnail opens the existing viewer using its formal message and tool receipt");
            NativeUi.Invoke(Button("ScreenshotPanelPrevious"));
            await WaitAsync(() => _api.Reads.Count == 2, "previous screenshot is loaded on demand");
            Complete(1); await WaitAsync(() => Image.Source is not null, "previous screenshot renders");
            NativeUi.Invoke(Button("ScreenshotPanelNext")); await SettleAsync();
            Check(_api.Reads.Count == 2 && ReferenceEquals(original, Image.Source), "returning to a loaded screenshot reuses the bounded thumbnail cache");
            _panel.SetPreviewEnabled(false); Check(Image.Source is null, "folding preview clears the displayed bitmap");
            _panel.SetPreviewEnabled(true); await SettleAsync();
            Check(_api.Reads.Count == 2 && Image.Source is not null, "reopening a loaded preview uses its cached thumbnail");

            row.Message.ToolActivities.Add(Screenshot("shot-a-late")); row.Refresh();
            await WaitAsync(() => _api.Reads.Count == 3, "a new completed receipt selects and lazily loads the newest screenshot");
            var lateA = _api.Reads[2]; var rowB = Message(_chatB, Screenshot("shot-b-1"));
            _panel.ShowConversation(_chatB, [rowB]);
            Check(lateA.Token.IsCancellationRequested && Image.Source is null, "chat switch cancels the old request and immediately clears its bitmap");
            await WaitAsync(() => _api.Reads.Count == 4, "new chat loads only its own screenshot");
            Complete(2); await SettleAsync();
            Check(Image.Source is null, "late response from the previous chat cannot populate the new chat");
            Complete(3); await WaitAsync(() => Image.Source is not null, "new chat thumbnail is displayed after its own response");

            rowB.Message.ToolActivities.Add(Screenshot("shot-b-cancel")); rowB.Refresh();
            await WaitAsync(() => _api.Reads.Count == 5, "pending new screenshot has one archive request");
            _panel.SetPreviewEnabled(false); Complete(4); await SettleAsync();
            Check(_api.Reads[4].Token.IsCancellationRequested && Image.Source is null && _api.Reads.Count == 5, "folding cancels pending decoding and performs no background downloads");
            _panel.SetPreviewEnabled(true); await WaitAsync(() => _api.Reads.Count == 6, "reopening retries only the previously cancelled archive read");
            Complete(5, "not-base64");
            await WaitAsync(() => Element<TextBlock>("ScreenshotPanelNotice").Text == "图片无法预览。" && Button("ScreenshotPanelOpen").IsEnabled, "malformed PNG fails without publishing image data and permits retry");
            NativeUi.Invoke(Button("ScreenshotPanelOpen")); await WaitAsync(() => _api.Reads.Count == 7, "clicking the preview retries archive reading without invoking a screenshot tool");
            Complete(6); await WaitAsync(() => Image.Source is not null, "archive-only retry recovers the screenshot");
            int reads = _api.Reads.Count; var retained = Image.Source;
            UiText.Initialize("en"); await SettleAsync();
            Check(Element<TextBlock>("ScreenshotPanelTitle").Text == "Screenshot" && _api.Reads.Count == reads && ReferenceEquals(retained, Image.Source), "language changes update the panel immediately without image reload");
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "panel-wide-en.png"));
            UiText.Initialize("zh-CN");

            var many = Message(_chatB, Enumerable.Range(0, 4).Select(i => Screenshot("bounded-" + i)).ToArray());
            _panel.ShowConversation(_chatB, [many]); await WaitAsync(() => _api.Reads.Count == reads + 1, "multiple stored screenshots download only the selected latest image");
            Complete(reads); await WaitAsync(() => Image.Source is not null, "latest image in a new gallery renders");
            for (int i = 1; i <= 3; i++)
            {
                NativeUi.Invoke(Button("ScreenshotPanelPrevious"));
                await WaitAsync(() => _api.Reads.Count == reads + i + 1, "manual selection reads one additional image " + i);
                Complete(reads + i); await WaitAsync(() => Image.Source is not null, "manual selection renders image " + i);
            }
            var cache = (IDictionary)typeof(ConversationScreenshotsPanel).GetField("_cache", BindingFlags.NonPublic | BindingFlags.Instance)!.GetValue(_panel)!;
            Check(cache.Count == 3, "decoded thumbnail cache evicts old images at its three-image limit");
            _layout.ColumnDefinitions[1].Width = new GridLength(190); await SettleAsync();
            Check(_panel.ActualWidth <= 162 && Image.ActualWidth <= _panel.ActualWidth &&
                Element<TextBlock>("ScreenshotPanelPosition").Text == "1 / 4", "narrow sidebar keeps the thumbnail and navigation within its bounds");
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "panel-narrow-zh.png"));

            many.Message.ToolActivities.Add(Screenshot("dispose-pending")); many.Refresh();
            await WaitAsync(() => _api.Reads.Count == reads + 5, "one pending archive read exists before disposal");
            var disposedRead = _api.Reads[^1]; int notifications = changes;
            _panel.Dispose(); Complete(_api.Reads.Count - 1); many.Refresh(); UiText.Initialize("en"); await SettleAsync();
            Check(disposedRead.Token.IsCancellationRequested && Image.Source is null && changes == notifications, "disposing cancels pending work and releases message and language subscriptions");
            Check(_unhandled is null && row.Message.Content.Contains("已读取页面") && !row.Message.Content.Contains(_png), "UI lifecycle has no unhandled exceptions and never rewrites formal model text with image bytes");
            int passed = _checks.Count(check => check.StartsWith("PASS ", StringComparison.Ordinal));
            File.AppendAllText(ResultPath, string.Join("\n", _checks) + $"\nPASS: {passed} screenshot panel UI checks.\nPreviews: {_directory}\n");
        }
        catch (Exception error) { File.AppendAllText(ResultPath, string.Join("\n", _checks) + "\nFAIL: " + error + "\n"); Environment.ExitCode = 1; }
        finally { _panel.Dispose(); _window.Close(); Exit(); }
    }

    private void CheckSources()
    {
        var good = Screenshot("good"); Guid messageId = Guid.NewGuid();
        var sources = ConversationScreenshotSources.Collect(_chatA, [
            new(_chatA, messageId, "assistant", [good, good, Screenshot("bad-hash", reference: new(Guid.NewGuid(), 1, "invalid")),
                Screenshot("unknown", "unknown"), Screenshot("failed", "error")]),
            new(_chatB, Guid.NewGuid(), "assistant", [Screenshot("other-chat")]),
            new(_chatA, Guid.NewGuid(), "user", [Screenshot("user-spoof")]),
            new(_chatA, Guid.Empty, "assistant", [Screenshot("empty-message")])]);
        Check(sources.Length == 1 && sources[0].Tool == good, "projection rejects wrong chat, user text, invalid identities, failed receipts, and duplicates");
        Check(!ConversationScreenshotSources.IsValidReference(new(Guid.NewGuid(), 8 * 1024 * 1024 + 1, new string('a', 64))), "oversized archive reference is rejected before download");
        var longIds = ConversationScreenshotSources.Collect(_chatA, [new(_chatA, Guid.NewGuid(), "assistant",
            [Screenshot(new string('a', 129)), Screenshot(new string('b', 200)), Screenshot(new string('c', 201))])]);
        Check(longIds.Length == 2, "formal call IDs up to 200 characters remain visible while oversized IDs are rejected");
    }

    private async Task CheckDecoderAsync()
    {
        var block = JsonSerializer.SerializeToElement(new { mimeType = "image/png", data = _png });
        var image = await ToolResultImageDecoder.DecodeAsync(block, pngOnly: true, maximumPreviewDimension: 128);
        Check(image.DecodePixelType == Microsoft.UI.Xaml.Media.Imaging.DecodePixelType.Physical && image.DecodePixelWidth == 128 && image.DecodePixelHeight == 64,
            "preview decoding requests bounded physical dimensions rather than retaining full screenshots");
        foreach (var format in new[] { (BitmapEncoder.JpegEncoderId, "image/jpeg"), (BitmapEncoder.GifEncoderId, "image/gif") })
        {
            string bytes = await EncodeAsync(format.Item1, 32, 16);
            var ordinary = await ToolResultImageDecoder.DecodeAsync(JsonSerializer.SerializeToElement(new { mimeType = format.Item2, data = bytes }));
            Check(ordinary.PixelWidth == 32 && ordinary.PixelHeight == 16, "shared viewer decoder preserves " + format.Item2 + " support");
        }
        await RejectAsync(() => ToolResultImageDecoder.DecodeAsync(block, maximumImageBytes: 1), "encoded image limit rejects oversized bytes before allocating a bitmap");
        await RejectAsync(() => ToolResultImageDecoder.DecodeAsync(block, maximumPixels: 100), "pixel limit rejects excessive dimensions");
        using var cancellation = new CancellationTokenSource(); cancellation.Cancel();
        try { await ToolResultImageDecoder.DecodeAsync(block, cancellation.Token); throw new InvalidOperationException("Cancelled decoding completed."); }
        catch (OperationCanceledException) { Check(true, "cancelled image decoding does not publish a bitmap"); }
    }
    private async Task RejectAsync(Func<Task<Microsoft.UI.Xaml.Media.Imaging.BitmapImage>> action, string label)
    { try { await action(); throw new InvalidOperationException("Expected decoder rejection: " + label); } catch (InvalidDataException) { Check(true, label); } }

    private static async Task<string> EncodeAsync(Guid codec, uint width, uint height)
    {
        byte[] pixels = new byte[checked((int)(width * height * 4))];
        for (uint y = 0; y < height; y++) for (uint x = 0; x < width; x++)
        {
            int offset = checked((int)((y * width + x) * 4));
            byte shade = y < height / 9 ? (byte)210 : x < width / 5 ? (byte)238 : (byte)250;
            if (x > width / 4 && x < width * 9 / 10 && y > height / 4 && y % 24 < 8) shade = 180;
            pixels[offset] = shade; pixels[offset + 1] = shade; pixels[offset + 2] = shade; pixels[offset + 3] = 255;
        }
        using var stream = new InMemoryRandomAccessStream();
        var encoder = await BitmapEncoder.CreateAsync(codec, stream);
        encoder.SetPixelData(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Ignore, width, height, 96, 96, pixels);
        await encoder.FlushAsync(); stream.Seek(0);
        using var output = new MemoryStream(); await stream.AsStreamForRead().CopyToAsync(output);
        return Convert.ToBase64String(output.ToArray());
    }
}

internal sealed record ScreenshotRead(Guid Conversation, ToolResultReference Reference, CancellationToken Token,
    TaskCompletionSource<ToolResultResponse> Completion);
internal sealed class FakeScreenshotApi : IAgentApi
{
    public List<ScreenshotRead> Reads { get; } = [];
    public Task<ToolResultResponse> GetToolResultAsync(Guid conversationId, ToolResultReference reference, CancellationToken cancellationToken = default)
    {
        var completion = new TaskCompletionSource<ToolResultResponse>(TaskCreationOptions.RunContinuationsAsynchronously);
        Reads.Add(new(conversationId, reference, cancellationToken, completion));
        // Deliberately ignore cancellation so the actual control must defend against late results.
        // 有意忽略取消，让真实控件必须防御晚到的结果。
        return completion.Task;
    }
    public Task<AgentConfig> GetConfigAsync(CancellationToken cancellationToken = default) => throw new NotSupportedException();
    public Task<AgentConfig> SaveConfigAsync(AgentConfigSaveRequest request, CancellationToken cancellationToken = default) => throw new NotSupportedException();
    public Task<AgentSkill[]> GetSkillsAsync(Guid? conversationId = null, CancellationToken cancellationToken = default) => throw new NotSupportedException();
    public Task<AgentSkillDetail> GetSkillAsync(string id, Guid? conversationId = null, CancellationToken cancellationToken = default) => throw new NotSupportedException();
    public Task<AgentToolsResponse> GetToolsAsync(CancellationToken cancellationToken = default) => throw new NotSupportedException();
    public Task<AgentToolsResponse> RefreshMcpAsync(Guid? conversationId = null, CancellationToken cancellationToken = default) => throw new NotSupportedException();
    public Task<AgentApprovalResponse> SubmitApprovalAsync(AgentApprovalRequest request, CancellationToken cancellationToken = default) => throw new NotSupportedException();
}
