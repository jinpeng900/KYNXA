using System.Diagnostics;
using KYNXA_Desktop.Services;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Bound floating notice text while keeping its complete message available to the native tooltip and automation.
/// 限制悬浮提示的正文高度，原生提示和辅助功能继续保留完整消息。
/// </summary>
public sealed class CompactInfoBar : InfoBar
{
    internal static readonly TimeSpan DismissDelay = TimeSpan.FromSeconds(5);
    private readonly Stopwatch _dismissElapsed = new();
    private DispatcherQueueTimer? _dismissTimer;
    private TimeSpan _remaining = DismissDelay;
    private bool _pointerInside, _focusInside, _isAutoDismissPaused;
    private UIElement? _pointerRoot;
    private readonly PointerEventHandler _outsidePointer;

    public CompactInfoBar()
    {
        _outsidePointer = DismissErrorOnOutsidePointer;
        DefaultStyleKey = typeof(InfoBar);
        IsClosable = true;
        Resources["InfoBarErrorSeverityBackgroundBrush"] = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        RegisterPropertyChangedCallback(SeverityProperty, (_, _) =>
        {
            RefreshMessagePresentation();
            UpdateAutoDismiss();
        });
        RegisterPropertyChangedCallback(TitleProperty, (_, _) => RefreshMessagePresentation());
        RegisterPropertyChangedCallback(IsOpenProperty, (_, _) =>
        {
            if (IsOpen) RestartAutoDismiss();
            else
            {
                PauseDismissClock();
                _pointerInside = _focusInside = false;
            }
        });
        Loaded += (_, _) =>
        {
            if (_pointerRoot is null && XamlRoot?.Content is UIElement root)
            { _pointerRoot = root; root.AddHandler(UIElement.PointerPressedEvent, _outsidePointer, true); }
            RestartAutoDismiss();
        };
        Unloaded += (_, _) =>
        {
            PauseDismissClock();
            if (_dismissTimer is not null) _dismissTimer.Tick -= DismissTimerTick;
            _dismissTimer = null;
            _pointerInside = _focusInside = false;
            _pointerRoot?.RemoveHandler(UIElement.PointerPressedEvent, _outsidePointer);
            _pointerRoot = null;
        };
        PointerEntered += (_, _) => { _pointerInside = true; UpdateAutoDismiss(); };
        PointerExited += (_, _) => { _pointerInside = false; UpdateAutoDismiss(); };
        GotFocus += (_, _) => { _focusInside = true; UpdateAutoDismiss(); };
        LostFocus += (_, _) => DispatcherQueue.TryEnqueue(() =>
        {
            if (!IsLoaded || !IsOpen) return;
            _focusInside = HasFocusWithin();
            UpdateAutoDismiss();
        });
    }

    internal bool IsAutoDismissPaused
    {
        get => _isAutoDismissPaused;
        set
        {
            if (_isAutoDismissPaused == value) return;
            _isAutoDismissPaused = value;
            UpdateAutoDismiss();
        }
    }

    internal void RestartAutoDismiss()
    {
        PauseDismissClock();
        _remaining = DismissDelay;
        _focusInside = HasFocusWithin();
        UpdateAutoDismiss();
    }

    private bool HasFocusWithin()
    {
        if (XamlRoot is null) return false;
        for (var element = FocusManager.GetFocusedElement(XamlRoot) as DependencyObject;
            element is not null; element = VisualTreeHelper.GetParent(element))
            if (ReferenceEquals(element, this)) return true;
        return false;
    }

    private void PauseDismissClock()
    {
        _dismissTimer?.Stop();
        if (!_dismissElapsed.IsRunning) return;
        _remaining -= _dismissElapsed.Elapsed;
        _dismissElapsed.Reset();
    }

    private void UpdateAutoDismiss()
    {
        PauseDismissClock();
        if (!IsLoaded || !IsOpen || _pointerInside || _focusInside || _isAutoDismissPaused) return;
        if (_remaining <= TimeSpan.Zero) { IsOpen = false; return; }
        if (_dismissTimer is null)
        {
            _dismissTimer = DispatcherQueue.CreateTimer();
            _dismissTimer.IsRepeating = false;
            _dismissTimer.Tick += DismissTimerTick;
        }
        // Charge only visible idle time; interaction pauses the remaining delay instead of restarting it.
        // 只累计可见空闲时间；交互时暂停剩余倒计时，而不是重新计满五秒。
        _dismissTimer.Interval = TimeSpan.FromMilliseconds(Math.Max(1, _remaining.TotalMilliseconds));
        _dismissElapsed.Restart();
        _dismissTimer.Start();
    }

    private void DismissTimerTick(DispatcherQueueTimer sender, object args)
    {
        if (!ReferenceEquals(sender, _dismissTimer)) return;
        // Recheck elapsed time so a queued tick from an earlier notice cannot dismiss its replacement.
        // 再核对实际经过时间，避免上一条提示排队中的回调关闭替换后的新提示。
        UpdateAutoDismiss();
    }

    protected override void OnApplyTemplate()
    {
        base.OnApplyTemplate();
        RefreshMessagePresentation();
    }

    private void RefreshMessagePresentation()
    {
        if (GetTemplateChild("Title") is TextBlock title)
            title.Visibility = string.IsNullOrEmpty(Title) ? Visibility.Collapsed : Visibility.Visible;
        if (GetTemplateChild("Message") is not TextBlock message) return;
        message.MaxLines = 1;
        message.FontSize = (double)Application.Current.Resources["KynxaBodyFontSize"];
        message.TextWrapping = TextWrapping.NoWrap;
        message.TextTrimming = TextTrimming.CharacterEllipsis;
        message.Foreground = AppearanceService.GetBrush(Severity == InfoBarSeverity.Error ? "KynxaErrorBrush" : "KynxaTextBrush");
        if (ToolTipService.GetToolTip(message) is not ToolTip)
        {
            var tooltip = new ToolTip();
            tooltip.SetBinding(ContentControl.ContentProperty, new Binding { Source = this, Path = new PropertyPath(nameof(Message)) });
            ToolTipService.SetToolTip(message, tooltip);
        }
    }

    private void DismissErrorOnOutsidePointer(object sender, PointerRoutedEventArgs args)
    {
        if (!IsOpen || Severity != InfoBarSeverity.Error) return;
        for (DependencyObject? element = args.OriginalSource as DependencyObject; element is not null;
            element = VisualTreeHelper.GetParent(element))
            if (ReferenceEquals(element, this)) return;
        IsOpen = false;
    }
}
