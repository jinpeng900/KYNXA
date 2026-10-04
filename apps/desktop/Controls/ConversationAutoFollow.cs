using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.System;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Keep a reply at the bottom until the reader deliberately scrolls away.
/// 回复保持跟随底部，直到用户主动向上阅读。
/// </summary>
internal sealed class ConversationAutoFollow
{
    private readonly ScrollViewer _scroll;
    private bool _following = true;
    private bool _queued;
    private bool _applying;
    private double _lastOffset;
    private double _lastExtent = -1;
    private double _lastViewport = -1;

    public ConversationAutoFollow(ScrollViewer scroll)
    {
        _scroll = scroll;
        scroll.ViewChanged += ViewChanged;
        scroll.LayoutUpdated += (_, _) =>
        {
            if (TextSelectionAutoScroll.HasActiveSelection) _following = false;
            if (Math.Abs(scroll.ScrollableHeight - _lastExtent) < 0.5 && Math.Abs(scroll.ViewportHeight - _lastViewport) < 0.5) return;
            _lastExtent = scroll.ScrollableHeight;
            _lastViewport = scroll.ViewportHeight;
            ContentChanged();
        };
        scroll.AddHandler(UIElement.PointerWheelChangedEvent, new PointerEventHandler((_, e) =>
        {
            if (e.GetCurrentPoint(scroll).Properties.MouseWheelDelta > 0) _following = false;
        }), true);
        scroll.AddHandler(UIElement.PointerPressedEvent, new PointerEventHandler((_, e) =>
        {
            for (DependencyObject? node = e.OriginalSource as DependencyObject; node is not null && node != scroll; node = VisualTreeHelper.GetParent(node))
                if (node is ScrollBar) { _following = false; break; }
        }), true);
        scroll.AddHandler(UIElement.KeyDownEvent, new KeyEventHandler((_, e) =>
        {
            if (e.Key is VirtualKey.Up or VirtualKey.PageUp or VirtualKey.Home) _following = false;
        }), true);
    }

    public void OpenChat()
    {
        _following = true;
        _lastOffset = _scroll.VerticalOffset;
        ContentChanged();
    }

    public void BeforeSend() => _following = _scroll.ScrollableHeight - _scroll.VerticalOffset <= 48;

    public void ContentChanged()
    {
        if (TextSelectionAutoScroll.HasActiveSelection) _following = false;
        if (!_following || _queued) return;
        _queued = true;
        _scroll.DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () =>
        {
            _queued = false;
            if (!_following || !_scroll.IsLoaded || TextSelectionAutoScroll.HasActiveSelection) return;
            _applying = true;
            try
            {
                _scroll.UpdateLayout();
                _scroll.ChangeView(null, _scroll.ScrollableHeight, null, disableAnimation: true);
                _lastOffset = _scroll.VerticalOffset;
            }
            finally { _applying = false; }
        });
    }

    private void ViewChanged(object? sender, ScrollViewerViewChangedEventArgs args)
    {
        double offset = _scroll.VerticalOffset;
        if (!_applying && !TextSelectionAutoScroll.HasActiveSelection)
        {
            // Layout shrink can clamp the offset without any action from the reader.
            // 布局缩小会自行限制滚动偏移，不能据此推断用户已主动滚动。
            double expectedOffset = Math.Min(_lastOffset, _scroll.ScrollableHeight);
            if (offset < expectedOffset - 1 && _scroll.ScrollableHeight - offset > 4) _following = false;
            else if (offset > _lastOffset + 1 && _scroll.ScrollableHeight - offset <= 4) _following = true;
        }
        _lastOffset = offset;
    }
}
