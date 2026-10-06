using KYNXA_Desktop.Services;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;
using Windows.System;

namespace KYNXA_Desktop.Views;

/// <summary>
/// Full-screen presentation of one formal screenshot archive. It owns no conversation or tool execution state.
/// 全屏展示一张正式归档截图；不拥有聊天状态或工具执行状态。
/// </summary>
public sealed class ScreenshotViewerWindow : Window, IDisposable
{
    private readonly IAgentApi _api;
    private readonly ConversationScreenshotSource _source;
    private readonly CancellationTokenSource _lifetime;
    private readonly CancellationTokenRegistration _cancellation;
    private readonly TaskCompletionSource _completion = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private readonly Grid _root = new() { Name = "ScreenshotViewerRoot", Background = new SolidColorBrush(Microsoft.UI.Colors.White) };
    private readonly Image _image = new() { Name = "ScreenshotViewerImage", Stretch = Stretch.Uniform };
    private readonly ScrollViewer _scroll = new()
    {
        Name = "ScreenshotViewerViewport", ZoomMode = ZoomMode.Enabled, MinZoomFactor = 0.1f, MaxZoomFactor = 16,
        HorizontalScrollMode = ScrollMode.Enabled, VerticalScrollMode = ScrollMode.Enabled,
        HorizontalScrollBarVisibility = ScrollBarVisibility.Auto, VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
        HorizontalContentAlignment = HorizontalAlignment.Center, VerticalContentAlignment = VerticalAlignment.Center
    };
    private readonly TextBlock _notice = new() { Name = "ScreenshotViewerNotice", TextWrapping = TextWrapping.Wrap,
        HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(24) };
    private readonly Button _fit = new() { Name = "ScreenshotViewerFit", MinWidth = 0 };
    private readonly Button _actual = new() { Name = "ScreenshotViewerActual", Content = "100%", MinWidth = 0 };
    private readonly Button _retry = new() { Name = "ScreenshotViewerRetry", MinWidth = 0, IsEnabled = false, Visibility = Visibility.Collapsed };
    private readonly Button _close = new() { Name = "ScreenshotViewerClose", Content = new FontIcon { Glyph = "\uE8BB", FontSize = 12 }, MinWidth = 0 };
    private DecodedToolImage? _decoded;
    private XamlRoot? _observedRoot;
    private Point? _dragStart;
    private double _dragHorizontal, _dragVertical;
    private double _imageBaseScale = 1;
    private bool _fitToWindow = true, _shown, _closed, _disposed, _busy;
    private string _noticeKey = "正在读取工具结果…";

    public ScreenshotViewerWindow(IAgentApi api, ConversationScreenshotSource source, CancellationToken cancellationToken = default)
    {
        _api = api; _source = source; _lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        _root.RowDefinitions.Add(new() { Height = new GridLength(44) });
        _root.RowDefinitions.Add(new() { Height = new GridLength(1, GridUnitType.Star) });
        var toolbar = new Grid { Margin = new Thickness(16, 4, 8, 4) };
        toolbar.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        toolbar.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        var title = new TextBlock { Name = "ScreenshotViewerTitle", VerticalAlignment = VerticalAlignment.Center };
        UiLocalization.Bind(title, TextBlock.TextProperty, "截图预览"); toolbar.Children.Add(title);
        var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        foreach (var button in new[] { _retry, _fit, _actual, _close })
        {
            if (Application.Current.Resources.TryGetValue("KynxaQuietButtonStyle", out var resource) && resource is Style quiet) button.Style = quiet;
            actions.Children.Add(button);
        }
        Grid.SetColumn(actions, 1); toolbar.Children.Add(actions); _root.Children.Add(toolbar);
        _scroll.Content = _image; Grid.SetRow(_scroll, 1); _root.Children.Add(_scroll);
        Grid.SetRow(_notice, 1); _root.Children.Add(_notice); _notice.IsHitTestVisible = false;
        Content = _root;
        _fit.Click += FitClicked; _actual.Click += ActualClicked; _close.Click += CloseClicked;
        _retry.Click += RetryClicked;
        AutomationProperties.SetLiveSetting(_notice, AutomationLiveSetting.Polite);
        _scroll.SizeChanged += ViewportChanged;
        _scroll.AddHandler(UIElement.PointerWheelChangedEvent, new PointerEventHandler(WheelChanged), true);
        _scroll.AddHandler(UIElement.PointerPressedEvent, new PointerEventHandler(DragPressed), true);
        _scroll.PointerMoved += DragMoved; _scroll.PointerReleased += DragEnded; _scroll.PointerCanceled += DragEnded;
        _scroll.PointerCaptureLost += DragEnded;
        _root.KeyDown += RootKeyDown; _root.Loaded += RootLoaded;
        UiText.LanguageChanged += LanguageChanged; Closed += WindowClosed;
        RefreshLanguage();
        _cancellation = _lifetime.Token.Register(() => DispatcherQueue.TryEnqueue(() => { if (!_closed) Close(); }));
    }

    public Task ShowAsync()
    {
        if (_shown) return _completion.Task;
        _shown = true;
        if (_lifetime.IsCancellationRequested) { Close(); return _completion.Task; }
        AppWindow.SetPresenter(AppWindowPresenterKind.FullScreen); Activate();
        _ = LoadAsync();
        return _completion.Task;
    }

    private async Task LoadAsync()
    {
        if (_closed || _disposed || _busy || _lifetime.IsCancellationRequested) return;
        // A retry reads the same validated archive identity. It never invokes a new screenshot operation.
        // 重试只读取同一个已验证的归档身份，绝不重新执行截图操作。
        _busy = true; _retry.IsEnabled = false;
        _noticeKey = "正在读取工具结果…"; RefreshLanguage(); _notice.Visibility = Visibility.Visible;
        var token = _lifetime.Token;
        try
        {
            var archive = await ConversationScreenshotLoader.LoadArchiveAsync(_api, _source, token);
            var decoded = await archive.DecodeAsync(0, token);
            if (_closed || token.IsCancellationRequested) return;
            _decoded = decoded; _image.Source = decoded.Bitmap; _notice.Visibility = Visibility.Collapsed;
            _retry.Visibility = Visibility.Collapsed;
            UpdateImageSize(); FitImage(); _fit.IsEnabled = _actual.IsEnabled = true;
            _close.Focus(FocusState.Programmatic);
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested) { }
        catch (Exception)
        {
            if (!_closed && !token.IsCancellationRequested)
            { _noticeKey = "图片无法预览。"; _retry.Visibility = Visibility.Visible; RefreshLanguage(); _notice.Visibility = Visibility.Visible; }
        }
        finally
        {
            _busy = false;
            if (!_closed && !_disposed)
            {
                _retry.IsEnabled = _retry.Visibility == Visibility.Visible && !token.IsCancellationRequested;
                if (_retry.IsEnabled) _retry.Focus(FocusState.Programmatic);
            }
        }
    }

    private async void RetryClicked(object sender, RoutedEventArgs args)
    {
        if (_retry.IsEnabled) await LoadAsync();
    }

    private void RootLoaded(object sender, RoutedEventArgs args)
    {
        if (_closed) return;
        _observedRoot = _root.XamlRoot;
        if (_observedRoot is not null) _observedRoot.Changed += RootChanged;
        UpdateImageSize(); FitImage();
        (_retry.IsEnabled ? _retry : _close).Focus(FocusState.Programmatic);
    }

    private void UpdateImageSize()
    {
        if (_decoded is null) return;
        double dpi = _root.XamlRoot?.RasterizationScale ?? 1;
        // Actual-size mode resets the base scale so each image pixel occupies one physical display pixel.
        // 实际尺寸模式重置基础缩放，使图片的每个像素对应一个物理显示像素。
        _image.Width = _decoded.OriginalPixelWidth / dpi * _imageBaseScale;
        _image.Height = _decoded.OriginalPixelHeight / dpi * _imageBaseScale;
    }

    private void FitImage()
    {
        if (_decoded is null || _scroll.ViewportWidth <= 0 || _scroll.ViewportHeight <= 0) return;
        double dpi = _root.XamlRoot?.RasterizationScale ?? 1;
        double fit = Math.Min(_scroll.ViewportWidth * dpi / _decoded.OriginalPixelWidth,
            _scroll.ViewportHeight * dpi / _decoded.OriginalPixelHeight);
        // WinUI refuses zoom factors below 0.1. Scale layout for very long pages without resampling the original bitmap.
        // WinUI 不接受低于 0.1 的缩放；超长页面通过调整布局适配，原始位图不重采样。
        _imageBaseScale = Math.Min(1, fit / _scroll.MinZoomFactor);
        _scroll.MaxZoomFactor = (float)(16 / _imageBaseScale); UpdateImageSize();
        float factor = (float)Math.Clamp(fit / _imageBaseScale, _scroll.MinZoomFactor, _scroll.MaxZoomFactor);
        _scroll.ChangeView(0, 0, factor, disableAnimation: true);
    }

    private void FitClicked(object sender, RoutedEventArgs args) { _fitToWindow = true; FitImage(); }
    private void ActualClicked(object sender, RoutedEventArgs args)
    {
        _fitToWindow = false; _imageBaseScale = 1; _scroll.MaxZoomFactor = 16;
        UpdateImageSize(); _scroll.UpdateLayout(); _scroll.ChangeView(0, 0, 1, disableAnimation: true);
    }
    private void CloseClicked(object sender, RoutedEventArgs args) => Close();
    private void ViewportChanged(object sender, SizeChangedEventArgs args) { if (_fitToWindow) FitImage(); }
    private void RootChanged(XamlRoot sender, XamlRootChangedEventArgs args) { UpdateImageSize(); if (_fitToWindow) FitImage(); }

    private void WheelChanged(object sender, PointerRoutedEventArgs args)
    {
        if (_decoded is null) return;
        var point = args.GetCurrentPoint(_scroll);
        float previous = _scroll.ZoomFactor;
        float next = (float)Math.Clamp(previous * Math.Pow(1.12, point.Properties.MouseWheelDelta / 120d), _scroll.MinZoomFactor, _scroll.MaxZoomFactor);
        _fitToWindow = false;
        double horizontal = (_scroll.HorizontalOffset + point.Position.X) / previous * next - point.Position.X;
        double vertical = (_scroll.VerticalOffset + point.Position.Y) / previous * next - point.Position.Y;
        _scroll.ChangeView(Math.Max(0, horizontal), Math.Max(0, vertical), next, disableAnimation: true);
        args.Handled = true;
    }

    private void DragPressed(object sender, PointerRoutedEventArgs args)
    {
        var point = args.GetCurrentPoint(_scroll);
        if (_decoded is null || point.Position.X >= _scroll.ViewportWidth || point.Position.Y >= _scroll.ViewportHeight ||
            !point.Properties.IsLeftButtonPressed || !_scroll.CapturePointer(args.Pointer)) return;
        _dragStart = point.Position;
        _dragHorizontal = _scroll.HorizontalOffset; _dragVertical = _scroll.VerticalOffset; args.Handled = true;
    }
    private void DragMoved(object sender, PointerRoutedEventArgs args)
    {
        if (_dragStart is not Point start) return;
        var point = args.GetCurrentPoint(_scroll).Position;
        _scroll.ChangeView(Math.Max(0, _dragHorizontal + start.X - point.X), Math.Max(0, _dragVertical + start.Y - point.Y), null, true);
        args.Handled = true;
    }
    private void DragEnded(object sender, PointerRoutedEventArgs args)
    { _dragStart = null; _scroll.ReleasePointerCapture(args.Pointer); }
    private void RootKeyDown(object sender, KeyRoutedEventArgs args)
    { if (args.Key == VirtualKey.Escape) { args.Handled = true; Close(); } }

    private void LanguageChanged(object? sender, EventArgs args)
    { if (DispatcherQueue.HasThreadAccess) RefreshLanguage(); else DispatcherQueue.TryEnqueue(RefreshLanguage); }
    private void RefreshLanguage()
    {
        if (_closed) return;
        Title = UiText.Get("截图预览"); _fit.Content = UiText.Get("适应窗口"); _notice.Text = UiText.Get(_noticeKey);
        _retry.Content = UiText.Get("重新读取");
        _fit.IsEnabled = _actual.IsEnabled = _decoded is not null;
        AutomationProperties.SetName(_fit, UiText.Get("适应窗口"));
        AutomationProperties.SetName(_actual, UiText.Get("原始尺寸")); AutomationProperties.SetName(_close, UiText.Get("关闭"));
        ToolTipService.SetToolTip(_actual, UiText.Get("原始尺寸")); ToolTipService.SetToolTip(_close, UiText.Get("关闭"));
        AutomationProperties.SetName(_retry, UiText.Get("重新读取已保存的截图"));
        ToolTipService.SetToolTip(_retry, UiText.Get("重新读取已保存的截图"));
    }

    private void WindowClosed(object sender, WindowEventArgs args)
    {
        _closed = true; _lifetime.Cancel(); _dragStart = null; _scroll.ReleasePointerCaptures();
        _retry.IsEnabled = false;
        _image.Source = null; _decoded = null;
        if (_observedRoot is not null) _observedRoot.Changed -= RootChanged;
        _observedRoot = null; UiText.LanguageChanged -= LanguageChanged; _completion.TrySetResult();
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true; _retry.IsEnabled = false;
        if (!_closed) Close(); _retry.Click -= RetryClicked; _cancellation.Dispose(); _lifetime.Dispose();
    }
}
