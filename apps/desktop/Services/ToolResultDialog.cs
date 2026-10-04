using System.Text;
using System.Text.Json;
using KYNXA.Contracts;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.ApplicationModel.DataTransfer;

namespace KYNXA_Desktop.Services;

/// <summary>Loads bounded text pages on demand. Media is fetched only by an explicit action.</summary>
public sealed class ToolResultDialog : IDisposable
{
    private readonly IAgentApi _api;
    private readonly Guid _conversationId;
    private readonly ToolResultReference _reference;
    private readonly bool _screenshot;
    private readonly CancellationTokenSource _lifetime;
    private readonly TextBox _text = new() { Name = "ToolResultText", IsReadOnly = true, AcceptsReturn = true, TextWrapping = TextWrapping.Wrap, MaxHeight = 280 };
    private readonly TextBlock _status = new() { Name = "ToolResultStatus", TextWrapping = TextWrapping.Wrap };
    private readonly Button _more = new() { Name = "ToolResultMore" };
    private readonly Button _media = new() { Name = "ToolResultMedia" };
    private readonly StackPanel _resources = new() { Name = "ToolResultResources", Spacing = 8 };
    private readonly StringBuilder _loaded = new();
    private int _nextOffset;
    private bool _closed, _busy;
    private string _statusKey = "正在读取工具结果…";
    private string _statusDetails = string.Empty;
    public ContentDialog Dialog { get; }

    public ToolResultDialog(XamlRoot root, IAgentApi api, Guid conversationId, ToolActivity tool,
        CancellationToken cancellationToken = default)
    {
        _api = api;
        _conversationId = conversationId;
        _reference = tool.ResultRef ?? throw new ArgumentException("A result reference is required.");
        _screenshot = ConversationScreenshotSources.IsScreenshotTool(tool.Name);
        _lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        _text.Resources["TextControlBorderBrushFocused"] = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 136, 136, 136));
        _more.Style = _media.Style = (Style)Application.Current.Resources["KynxaQuietButtonStyle"];
        UiLocalization.Bind(_more, ContentControl.ContentProperty, "加载下一段");
        UiLocalization.Bind(_media, ContentControl.ContentProperty, _screenshot ? "查看截图" : "查看媒体与资源");
        _more.Click += async (_, _) => await LoadNextAsync();
        _media.Click += async (_, _) => await LoadResourcesAsync();
        var copy = new Button { Name = "ToolResultCopy", Style = _more.Style };
        UiLocalization.Bind(copy, ContentControl.ContentProperty, "复制已加载内容");
        copy.Click += (_, _) => { var data = new DataPackage(); data.SetText(_text.Text); Clipboard.SetContent(data); };
        var content = new StackPanel { Spacing = 10 };
        var name = new TextBlock { FontWeight = Microsoft.UI.Text.FontWeights.SemiBold };
        if (_screenshot) UiLocalization.Bind(name, TextBlock.TextProperty, "截图"); else name.Text = tool.Name;
        content.Children.Add(name);
        if (!_screenshot) content.Children.Add(new TextBlock { Text = $"{_reference.Bytes} bytes · SHA256 {_reference.Sha256}", TextWrapping = TextWrapping.Wrap });
        content.Children.Add(_status);
        if (!_screenshot) content.Children.Add(_text);
        var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        if (!_screenshot) { actions.Children.Add(_more); actions.Children.Add(copy); }
        actions.Children.Add(_media);
        content.Children.Add(actions); content.Children.Add(_resources);
        Dialog = new ContentDialog { XamlRoot = root, Content = new ScrollViewer { Content = content, MaxHeight = 520 },
            DefaultButton = ContentDialogButton.None, CloseButtonStyle = _more.Style };
        UiLocalization.Bind(Dialog, ContentDialog.TitleProperty, _screenshot ? "截图预览" : "工具结果详情");
        UiLocalization.Bind(Dialog, ContentDialog.CloseButtonTextProperty, "关闭");
        Dialog.Opened += async (_, _) => { if (_screenshot) await LoadResourcesAsync(); else await LoadNextAsync(); };
        Dialog.Closed += (_, _) => Dispose();
        UiText.LanguageChanged += LanguageChanged;
        LanguageChanged(null, EventArgs.Empty);
        // Keep media decoding bounded; regular results can still be read through text pages.
        _media.IsEnabled = _reference.Bytes <= 8 * 1024 * 1024;
        if (!_media.IsEnabled) AddNotice(_screenshot ? "截图过大，无法预览（最大 8 MB）。" : "结果过大，请通过分页查看文本；媒体预览限制为 8 MB。");
    }

    public async Task ShowAsync()
    {
        using var registration = _lifetime.Token.Register(() =>
        {
            if (!_closed) Dialog.DispatcherQueue.TryEnqueue(() => { if (!_closed) Dialog.Hide(); });
        });
        if (!_lifetime.IsCancellationRequested) await Dialog.ShowAsync();
    }

    private void LanguageChanged(object? sender, EventArgs e)
    {
        if (_closed) return;
        if (!Dialog.DispatcherQueue.HasThreadAccess) { Dialog.DispatcherQueue.TryEnqueue(() => LanguageChanged(sender, e)); return; }
        _status.Text = UiText.Get(_statusKey) + _statusDetails;
    }

    private void Status(string key, string details = "")
    { _statusKey = key; _statusDetails = details; LanguageChanged(null, EventArgs.Empty); }

    private async Task LoadNextAsync()
    {
        if (_closed || _busy || _lifetime.IsCancellationRequested) return;
        _busy = true; _more.IsEnabled = _media.IsEnabled = false;
        try
        {
            var page = await _api.GetToolResultPageAsync(_conversationId, _reference, _nextOffset, cancellationToken: _lifetime.Token);
            if (_closed || _lifetime.IsCancellationRequested) return;
            _loaded.Append(page.Text); _text.Text = _loaded.ToString(); _nextOffset = page.NextOffset;
            _more.Visibility = page.Truncated ? Visibility.Visible : Visibility.Collapsed;
            Status("已加载字符：", $" {_nextOffset} / {page.TotalCharacters}");
        }
        catch (OperationCanceledException) when (_lifetime.IsCancellationRequested) { }
        catch (Exception error) { if (!_closed) Status("工具结果读取失败，请重试。", error is GatewayApiException ? " " + error.Message : ""); }
        finally { if (!_closed) { _busy = false; _more.IsEnabled = true; _media.IsEnabled = _reference.Bytes <= 8 * 1024 * 1024; } }
    }

    private async Task LoadResourcesAsync()
    {
        if (_closed || _busy || _reference.Bytes > 8 * 1024 * 1024) return;
        _busy = true; _media.IsEnabled = _more.IsEnabled = false;
        try
        {
            var response = await _api.GetToolResultAsync(_conversationId, _reference, _lifetime.Token);
            if (_closed || _lifetime.IsCancellationRequested) return;
            _resources.Children.Clear();
            Status("已加载媒体与资源。");
            if (response.Result.ValueKind != JsonValueKind.Object || !response.Result.TryGetProperty("content", out var blocks) ||
                blocks.ValueKind != JsonValueKind.Array) { AddNotice("此结果没有可预览的媒体或资源。"); return; }
            int count = 0;
            foreach (var block in blocks.EnumerateArray())
            {
                if (_closed || _lifetime.IsCancellationRequested) return;
                string type = StringField(block, "type");
                if (type == "text") continue;
                if (++count > 20) { AddNotice("媒体过多，仅显示前 20 项。"); break; }
                if (type == "image") await AddImageAsync(block);
                else if (type is "resource" or "resource_link")
                {
                    var resource = type == "resource" && block.TryGetProperty("resource", out var nested) ? nested : block;
                    AddLiteral($"{type} · {StringField(resource, "mimeType")}\n{StringField(resource, "uri")}\n{StringField(resource, "name")}");
                    if (resource.TryGetProperty("text", out var resourceText) && resourceText.ValueKind == JsonValueKind.String)
                        AddLiteral(resourceText.GetString()!);
                    if (resource.TryGetProperty("blob", out _)) AddNotice("此资源包含二进制内容，当前仅显示元信息。");
                }
                else { AddLiteral(type + " · " + StringField(block, "mimeType")); AddNotice("此媒体类型暂不支持直接预览。"); }
            }
            if (count == 0) AddNotice("此结果没有可预览的媒体或资源。");
        }
        catch (OperationCanceledException) when (_lifetime.IsCancellationRequested) { }
        catch (Exception error) { if (!_closed) Status("工具结果读取失败，请重试。", error is GatewayApiException ? " " + error.Message : ""); }
        finally { if (!_closed) { _busy = false; _media.IsEnabled = true; _more.IsEnabled = true; } }
    }

    private async Task AddImageAsync(JsonElement block)
    {
        string mime = StringField(block, "mimeType");
        if (!_screenshot) AddLiteral("image · " + mime);
        if (mime is not ("image/png" or "image/jpeg" or "image/gif" or "image/webp"))
        { AddNotice("此媒体类型暂不支持直接预览。"); return; }
        try
        {
            var bitmap = await ToolResultImageDecoder.DecodeAsync(block, _lifetime.Token);
            if (!_closed && !_lifetime.IsCancellationRequested) _resources.Children.Add(new Image { Source = bitmap, MaxHeight = 260 });
        }
        catch (OperationCanceledException) when (_lifetime.IsCancellationRequested) { }
        catch (Exception) { if (!_closed) AddNotice(_screenshot ? "图片无法预览。" : "图片无法预览，当前仅显示元信息。"); }
    }

    private static string StringField(JsonElement value, string name) => value.ValueKind == JsonValueKind.Object &&
        value.TryGetProperty(name, out var field) && field.ValueKind == JsonValueKind.String ? field.GetString() ?? "" : "";
    private void AddLiteral(string text)
    {
        var field = new TextBox { IsReadOnly = true, AcceptsReturn = true, TextWrapping = TextWrapping.Wrap, MaxHeight = 180, Text = text };
        field.Resources["TextControlBorderBrushFocused"] = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 136, 136, 136));
        _resources.Children.Add(field);
    }
    private void AddNotice(string key)
    { var label = new TextBlock { TextWrapping = TextWrapping.Wrap }; UiLocalization.Bind(label, TextBlock.TextProperty, key); _resources.Children.Add(label); }
    public void Dispose()
    {
        if (_closed) return;
        _closed = true; _lifetime.Cancel(); _lifetime.Dispose(); UiText.LanguageChanged -= LanguageChanged;
    }
}
