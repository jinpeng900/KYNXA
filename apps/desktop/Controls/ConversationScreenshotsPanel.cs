using System.ComponentModel;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using KYNXA.Contracts;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// A lazy, bounded gallery owned by the current conversation. Images are never put in chat text.
/// 当前聊天拥有的延迟加载、有容量上限的图片集合；截图不进入聊天正文。
/// </summary>
public sealed class ConversationScreenshotsPanel : Grid, IDisposable
{
    private const int MaximumCachedImages = 3;
    private const long MaximumCachedPixels = 16_000_000;
    private readonly TextBlock _title = new() { Name = "ScreenshotPanelTitle", FontSize = 12 };
    private readonly TextBlock _position = new() { Name = "ScreenshotPanelPosition", FontSize = 11, VerticalAlignment = VerticalAlignment.Center };
    private readonly TextBlock _notice = new() { Name = "ScreenshotPanelNotice", FontSize = 12, TextWrapping = TextWrapping.Wrap,
        HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(12) };
    private readonly Image _image = new() { Name = "ScreenshotPanelImage", Stretch = Stretch.Uniform,
        HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center };
    private readonly Border _surface;
    private readonly Grid _header;
    private readonly Microsoft.UI.Dispatching.DispatcherQueueTimer _resizeTimer;
    private readonly Button _open = new() { Name = "ScreenshotPanelOpen", Padding = new Thickness(0), BorderThickness = new Thickness(0),
        HorizontalContentAlignment = HorizontalAlignment.Stretch, VerticalContentAlignment = VerticalAlignment.Stretch };
    private readonly Button _previous = NavigationButton("ScreenshotPanelPrevious", "\uE76B");
    private readonly Button _next = NavigationButton("ScreenshotPanelNext", "\uE76C");
    private readonly StackPanel _navigation = new() { Orientation = Orientation.Horizontal, Spacing = 6 };
    private readonly Dictionary<ConversationMessageViewModel, ToolActivity[]> _activitySnapshots = [];
    private readonly Dictionary<string, CachedScreenshot> _cache = [];
    private readonly LinkedList<string> _cacheOrder = [];
    private IReadOnlyList<ConversationMessageViewModel> _messages = [];
    private ConversationScreenshotSource[] _screenshotSources = [];
    private IAgentApi? _api;
    private Guid? _conversationId;
    private CancellationTokenSource? _loadCancellation;
    private string? _loadingSourceIdentity;
    private XamlRoot? _observedRoot;
    private long _loadGeneration;
    private int _selectedSourceIndex;
    private bool _previewEnabled, _disposed, _tabbedMode;
    private string _noticeKey = "正在读取工具结果…";

    private sealed record CachedScreenshot(ArchivedScreenshot Archive, DecodedToolImage Image, int PreviewDimension)
    {
        public long DecodedPixels
        {
            get
            {
                double scale = Math.Min(1, PreviewDimension / (double)Math.Max(Image.OriginalPixelWidth, Image.OriginalPixelHeight));
                return Math.Max(1, (long)(Image.OriginalPixelWidth * scale)) * Math.Max(1, (long)(Image.OriginalPixelHeight * scale));
            }
        }
    }

    public bool HasScreenshots => _screenshotSources.Length > 0;
    public IReadOnlyList<ConversationScreenshotSource> Items => _screenshotSources;
    public ConversationScreenshotSource? SelectedSource => _screenshotSources.Length > 0 ? _screenshotSources[_selectedSourceIndex] : null;
    public event EventHandler? ScreenshotsChanged;
    public event EventHandler<ToolResultRequest>? ScreenshotOpenRequested;

    public ConversationScreenshotsPanel()
    {
        Visibility = Visibility.Collapsed;
        var resources = Application.Current.Resources;
        if (resources.TryGetValue("KynxaSecondaryTextBrush", out var foreground) && foreground is Brush secondary)
            _title.Foreground = _position.Foreground = _notice.Foreground = _previous.Foreground = _next.Foreground = secondary;
        if (resources.TryGetValue("KynxaCaptionFontSize", out var size) && size is double caption)
            _title.FontSize = _notice.FontSize = caption;
        if (resources.TryGetValue("KynxaCompactIconButtonStyle", out var iconResource) && iconResource is Style compact)
            _previous.Style = _next.Style = compact;
        if (Application.Current.Resources.TryGetValue("KynxaQuietButtonStyle", out var resource) && resource is Style quiet)
            _open.Style = quiet;
        _notice.IsHitTestVisible = false;
        var header = _header = new Grid { Margin = new Thickness(12, 9, 8, 5) };
        header.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        header.Children.Add(_title);
        _navigation.Children.Add(_previous); _navigation.Children.Add(_position); _navigation.Children.Add(_next);
        Grid.SetColumn(_navigation, 1); header.Children.Add(_navigation);
        var preview = new Grid { MinHeight = 74, Margin = new Thickness(8, 0, 8, 8) };
        _open.Content = _image; preview.Children.Add(_open); preview.Children.Add(_notice);
        Grid.SetRow(preview, 1);
        var body = new Grid(); body.RowDefinitions.Add(new() { Height = GridLength.Auto }); body.RowDefinitions.Add(new() { Height = GridLength.Auto });
        body.Children.Add(header); body.Children.Add(preview);
        _surface = new Border { Name = "ScreenshotPanelSurface", CornerRadius = new CornerRadius(10), Child = body,
            VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(0, 24, 0, 0) };
        if (resources.TryGetValue("KynxaReplySurfaceBrush", out var background) && background is Brush replySurface)
            _surface.Background = replySurface;
        Children.Add(_surface);
        _resizeTimer = DispatcherQueue.CreateTimer();
        _resizeTimer.Interval = TimeSpan.FromMilliseconds(180); _resizeTimer.IsRepeating = false;
        _resizeTimer.Tick += ResizeSettled;
        _previous.Click += (_, _) => Select(_selectedSourceIndex - 1);
        _next.Click += (_, _) => Select(_selectedSourceIndex + 1);
        _open.Click += (_, _) => OpenSelected();
        Loaded += PanelLoaded; Unloaded += PanelUnloaded; SizeChanged += PanelSizeChanged;
        UiText.LanguageChanged += LanguageChanged;
        RefreshLanguage();
    }

    private static Button NavigationButton(string name, string glyph) => new()
    {
        Name = name, Content = new FontIcon { Glyph = glyph, FontSize = 10 }, Width = 26, Height = 26, MinWidth = 0, MinHeight = 0,
        Padding = new Thickness(0), BorderThickness = new Thickness(0), Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent)
    };

    public void ConfigureApi(IAgentApi api)
    {
        ArgumentNullException.ThrowIfNull(api);
        if (_disposed || ReferenceEquals(_api, api)) return;
        _api = api; CancelLoading(); ClearImages(); RenderSelected();
    }

    public void ShowConversation(Guid? conversationId, IReadOnlyList<ConversationMessageViewModel> messages)
    {
        if (_disposed) return;
        ArgumentNullException.ThrowIfNull(messages);
        bool changed = _conversationId != conversationId;
        if (changed) { CancelLoading(); ClearImages(); _screenshotSources = []; _selectedSourceIndex = 0; }
        bool sameRows = !changed && _messages.Count == messages.Count && _messages.Zip(messages).All(pair => ReferenceEquals(pair.First, pair.Second));
        _conversationId = conversationId;
        if (sameRows && !_messages.Any(RememberActivities)) return;
        if (!sameRows)
        {
            foreach (var message in _messages) message.PropertyChanged -= MessageChanged;
            _activitySnapshots.Clear(); _messages = messages.ToArray();
            foreach (var message in _messages) { message.PropertyChanged += MessageChanged; RememberActivities(message); }
        }
        RefreshSources(force: changed);
    }

    public void SetPreviewEnabled(bool enabled)
    {
        if (_disposed || _previewEnabled == enabled) return;
        _previewEnabled = enabled;
        if (_tabbedMode) Visibility = enabled && HasScreenshots ? Visibility.Visible : Visibility.Collapsed;
        if (!enabled) { _resizeTimer.Stop(); CancelLoading(); _image.Source = null; _open.IsEnabled = false; }
        else RenderSelected();
    }

    public void SetTabbedMode(bool enabled)
    {
        if (_disposed || _tabbedMode == enabled) return;
        _tabbedMode = enabled;
        _header.Visibility = enabled ? Visibility.Collapsed : Visibility.Visible;
        _surface.Margin = new Thickness(0, 24, 0, 0);
        _surface.CornerRadius = new CornerRadius(enabled ? 0 : 10);
        _surface.VerticalAlignment = VerticalAlignment.Center;
        _surface.Background = enabled ? null : Application.Current.Resources["KynxaReplySurfaceBrush"] as Brush;
        Visibility = HasScreenshots && (!enabled || _previewEnabled) ? Visibility.Visible : Visibility.Collapsed;
        ResizePreview();
    }

    public bool SelectResult(Guid resultId)
    {
        int index = Array.FindIndex(_screenshotSources, source => source.Tool.ResultRef?.Id == resultId);
        if (_disposed || index < 0) return false;
        Select(index); return true;
    }

    public bool SelectSource(Guid messageId, string toolCallId)
    {
        int index = Array.FindIndex(_screenshotSources, source => source.MessageId == messageId && source.Tool.ToolCallId == toolCallId);
        if (_disposed || index < 0) return false;
        Select(index); return true;
    }

    private bool RememberActivities(ConversationMessageViewModel message)
    {
        var tools = message.ToolActivities;
        if (_activitySnapshots.TryGetValue(message, out var previous) && previous.Length == tools.Count &&
            previous.Select((tool, index) => ReferenceEquals(tool, tools[index])).All(equal => equal)) return false;
        _activitySnapshots[message] = tools.ToArray(); return true;
    }

    private void MessageChanged(object? sender, PropertyChangedEventArgs args)
    {
        if (_disposed || sender is not ConversationMessageViewModel message ||
            (args.PropertyName is not (null or "" or nameof(ConversationMessageViewModel.ToolActivities) or "ResultRef"))) return;
        if (!DispatcherQueue.HasThreadAccess) { DispatcherQueue.TryEnqueue(() => MessageChanged(sender, args)); return; }
        if (_messages.Contains(message) && RememberActivities(message)) RefreshSources();
    }

    private void RefreshSources(bool force = false)
    {
        var selected = SelectedSource;
        var next = ConversationScreenshotSources.Collect(_conversationId,
            _messages.Select(message => new ScreenshotMessage(message.ConversationId, message.Message.Id, message.Message.Role, message.ToolActivities)));
        bool changed = force || !_screenshotSources.Select(source => source.Identity).SequenceEqual(next.Select(source => source.Identity));
        _screenshotSources = next;
        if (!changed) return;
        int previousIndex = _tabbedMode && !force && selected is not null
            ? Array.FindIndex(next, source => source.MessageId == selected.MessageId && source.Tool.ToolCallId == selected.Tool.ToolCallId) : -1;
        _selectedSourceIndex = previousIndex >= 0 ? previousIndex : next.Length == 0 ? 0 : next.Length - 1;
        Visibility = HasScreenshots && (!_tabbedMode || _previewEnabled) ? Visibility.Visible : Visibility.Collapsed;
        ScreenshotsChanged?.Invoke(this, EventArgs.Empty);
        RenderSelected();
    }

    private void Select(int index)
    {
        if (_disposed || index < 0 || index >= _screenshotSources.Length || index == _selectedSourceIndex) return;
        _selectedSourceIndex = index; RenderSelected();
    }

    private void OpenSelected()
    {
        if (_disposed || !_previewEnabled || _screenshotSources.Length == 0) return;
        if (_image.Source is null) { RenderSelected(); return; }
        var source = _screenshotSources[_selectedSourceIndex];
        ScreenshotOpenRequested?.Invoke(this, new(source.ConversationId, source.MessageId, source.Tool));
    }

    private void RenderSelected()
    {
        _navigation.Visibility = !_tabbedMode && _screenshotSources.Length > 1 ? Visibility.Visible : Visibility.Collapsed;
        _position.Text = $"{(_screenshotSources.Length == 0 ? 0 : _selectedSourceIndex + 1)} / {_screenshotSources.Length}";
        _previous.IsEnabled = _selectedSourceIndex > 0; _next.IsEnabled = _selectedSourceIndex < _screenshotSources.Length - 1;
        // Shell can synchronously open the sidebar during ScreenshotsChanged. Keep the same in-flight read.
        // ScreenshotsChanged 可能让主界面同步打开侧栏；保留正在进行的同一次读取。
        if (!_disposed && _previewEnabled && IsLoaded && HasScreenshots && _loadCancellation is not null &&
            _loadingSourceIdentity == _screenshotSources[_selectedSourceIndex].Identity) return;
        CancelLoading(); _image.Source = null; _open.IsEnabled = false;
        _notice.Visibility = Visibility.Collapsed;
        if (_disposed || !_previewEnabled || !IsLoaded || !HasScreenshots || _api is null) return;
        var source = _screenshotSources[_selectedSourceIndex];
        int previewDimension = RequestedPreviewDimension();
        _cache.TryGetValue(source.Identity, out var cached);
        if (cached is not null)
        {
            Present(source.Identity, cached);
            if (cached.PreviewDimension >= previewDimension || Math.Max(cached.Image.OriginalPixelWidth, cached.Image.OriginalPixelHeight) <= cached.PreviewDimension) return;
        }
        else { _noticeKey = "正在读取工具结果…"; RefreshLanguage(); _notice.Visibility = Visibility.Visible; }
        var cancellation = _loadCancellation = new CancellationTokenSource();
        _loadingSourceIdentity = source.Identity;
        _ = LoadSelectedAsync(source, cached?.Archive, previewDimension, _loadGeneration, cancellation);
    }

    private async Task LoadSelectedAsync(ConversationScreenshotSource source, ArchivedScreenshot? archive, int previewDimension,
        long generation, CancellationTokenSource cancellation)
    {
        var token = cancellation.Token;
        bool decoded = false;
        try
        {
            archive ??= await ConversationScreenshotLoader.LoadArchiveAsync(_api!, source, token);
            var image = await archive.DecodeAsync(previewDimension, token);
            if (!IsCurrent(source.Identity, generation, token)) return;
            var cached = new CachedScreenshot(archive, image, previewDimension);
            _cache[source.Identity] = cached; _cacheOrder.Remove(source.Identity); _cacheOrder.AddLast(source.Identity);
            while (_cacheOrder.Count > MaximumCachedImages || (_cacheOrder.Count > 1 && _cache.Values.Sum(item => item.DecodedPixels) > MaximumCachedPixels))
            { string oldest = _cacheOrder.First!.Value; _cacheOrder.RemoveFirst(); _cache.Remove(oldest); }
            Present(source.Identity, cached);
            decoded = true;
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested) { }
        catch (Exception)
        {
            if (IsCurrent(source.Identity, generation, token))
            { _noticeKey = "图片无法预览。"; RefreshLanguage(); _notice.Visibility = Visibility.Visible; _open.IsEnabled = true; }
        }
        finally
        {
            if (ReferenceEquals(_loadCancellation, cancellation))
            {
                _loadCancellation = null; _loadingSourceIdentity = null; cancellation.Dispose();
                if (decoded && _cache.TryGetValue(source.Identity, out var cached) && cached.PreviewDimension < RequestedPreviewDimension()) QueuePreviewResize();
            }
        }
    }

    private bool IsCurrent(string identity, long generation, CancellationToken cancellation) => !_disposed && _previewEnabled && IsLoaded &&
        !cancellation.IsCancellationRequested && generation == _loadGeneration && _screenshotSources.Length > 0 && _screenshotSources[_selectedSourceIndex].Identity == identity;

    private void Present(string identity, CachedScreenshot screenshot)
    {
        _cacheOrder.Remove(identity); _cacheOrder.AddLast(identity); _image.Source = screenshot.Image.Bitmap;
        _open.IsEnabled = true; _notice.Visibility = Visibility.Collapsed; ResizePreview();
    }

    private void ResizePreview()
    {
        if (!HasScreenshots || !_cache.TryGetValue(_screenshotSources[_selectedSourceIndex].Identity, out var cached)) return;
        double scale = PreviewScale(cached.Image);
        _image.Width = cached.Image.OriginalPixelWidth * scale; _image.Height = cached.Image.OriginalPixelHeight * scale;
    }

    private double PreviewScale(DecodedToolImage image)
    {
        double width = Math.Max(1, ActualWidth - 16);
        double height = ActualHeight > 0 ? Math.Max(1, ActualHeight - (_tabbedMode ? 16 : 72)) : width;
        return Math.Min(width / image.OriginalPixelWidth, height / image.OriginalPixelHeight);
    }

    private int RequestedPreviewDimension()
    {
        double dimension = Math.Max(1, ActualWidth - 16);
        if (HasScreenshots && _cache.TryGetValue(_screenshotSources[_selectedSourceIndex].Identity, out var cached))
            dimension = Math.Max(cached.Image.OriginalPixelWidth, cached.Image.OriginalPixelHeight) * PreviewScale(cached.Image);
        double physical = dimension * (XamlRoot?.RasterizationScale ?? 1);
        return physical <= 1024 ? 1024 : physical <= 2048 ? 2048 : 4096;
    }

    private void QueuePreviewResize()
    { if (!_disposed && _previewEnabled && IsLoaded) { _resizeTimer.Stop(); _resizeTimer.Start(); } }
    private void PanelSizeChanged(object sender, SizeChangedEventArgs args) { ResizePreview(); QueuePreviewResize(); }
    private void RootChanged(XamlRoot sender, XamlRootChangedEventArgs args) { ResizePreview(); QueuePreviewResize(); }
    private void ResizeSettled(Microsoft.UI.Dispatching.DispatcherQueueTimer sender, object args)
    {
        if (!HasScreenshots || !_cache.TryGetValue(_screenshotSources[_selectedSourceIndex].Identity, out var cached) ||
            cached.PreviewDimension < RequestedPreviewDimension()) RenderSelected();
    }

    private void CancelLoading()
    { _loadGeneration++; _loadCancellation?.Cancel(); _loadCancellation?.Dispose(); _loadCancellation = null; _loadingSourceIdentity = null; }
    private void ClearImages() { _image.Source = null; _cache.Clear(); _cacheOrder.Clear(); }
    private void PanelLoaded(object sender, RoutedEventArgs args)
    {
        _observedRoot = XamlRoot;
        if (_observedRoot is not null) _observedRoot.Changed += RootChanged;
        RenderSelected();
    }
    private void PanelUnloaded(object sender, RoutedEventArgs args)
    {
        if (_observedRoot is not null) _observedRoot.Changed -= RootChanged;
        _observedRoot = null; _resizeTimer.Stop(); CancelLoading(); _image.Source = null;
    }
    private void LanguageChanged(object? sender, EventArgs args)
    { if (_disposed) return; if (DispatcherQueue.HasThreadAccess) RefreshLanguage(); else DispatcherQueue.TryEnqueue(RefreshLanguage); }
    private void RefreshLanguage()
    {
        if (_disposed) return;
        _title.Text = UiText.Get("截图"); _notice.Text = UiText.Get(_noticeKey);
        ToolTipService.SetToolTip(_previous, UiText.Get("上一张")); ToolTipService.SetToolTip(_next, UiText.Get("下一张"));
        AutomationProperties.SetName(_previous, UiText.Get("上一张")); AutomationProperties.SetName(_next, UiText.Get("下一张"));
        AutomationProperties.SetName(_open, UiText.Get("查看截图"));
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true; _resizeTimer.Stop(); _resizeTimer.Tick -= ResizeSettled;
        if (_observedRoot is not null) _observedRoot.Changed -= RootChanged;
        _observedRoot = null; CancelLoading(); ClearImages();
        foreach (var message in _messages) message.PropertyChanged -= MessageChanged;
        _messages = []; _screenshotSources = []; _activitySnapshots.Clear();
        UiText.LanguageChanged -= LanguageChanged; Loaded -= PanelLoaded; Unloaded -= PanelUnloaded; SizeChanged -= PanelSizeChanged;
    }
}
