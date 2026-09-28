using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.ApplicationModel.DataTransfer;
using Windows.Foundation;
using Windows.System;
using Windows.UI.Core;

namespace KYNXA_Desktop.Controls;

/// <summary>All message text surfaces in a viewport share one ordered selection.</summary>
internal sealed class TextSelectionAutoScroll
{
    private static readonly ConditionalWeakTable<ScrollViewer, ConversationSelection> Sessions = new();
    private readonly RichTextBlock _text;
    private ConversationSelection? _session;

    public TextSelectionAutoScroll(RichTextBlock text)
    {
        _text = text;
        text.Loaded += (_, _) => Attach();
        text.Unloaded += (_, _) => { _session?.Remove(text); _session = null; };
        text.AddHandler(UIElement.PointerPressedEvent, new PointerEventHandler(Pressed), true);
        var menu = new MenuFlyout();
        var copy = new MenuFlyoutItem { Text = "复制" };
        copy.Click += (_, _) => _session?.Copy(text);
        menu.Items.Add(copy);
        menu.Opening += (_, _) => copy.IsEnabled = (_session?.SelectedText.Length ?? 0) > 0 || text.SelectedText.Length > 0;
        text.ContextFlyout = menu;
    }

    private void Attach()
    {
        if (_session is not null) return;
        for (DependencyObject? parent = VisualTreeHelper.GetParent(_text); parent is not null; parent = VisualTreeHelper.GetParent(parent))
            if (parent is ScrollViewer scroll)
            {
                _session = Sessions.GetValue(scroll, viewer => new ConversationSelection(viewer));
                _session.Add(_text);
                return;
            }
    }

    private void Pressed(object sender, PointerRoutedEventArgs e)
    {
        Attach();
        _session?.Begin(_text, e);
    }

    public void Stop() => _session?.Invalidate(_text);
    internal string SelectedConversationText => _session?.SelectedText ?? _text.SelectedText;

    private sealed class ConversationSelection(ScrollViewer scroll)
    {
        private sealed record Range(RichTextBlock Text, TextPointer Start, TextPointer End);
        private readonly HashSet<RichTextBlock> _documents = [];
        private readonly Dictionary<RichTextBlock, TextHighlighter> _highlights = [];
        private List<Range> _selection = [];
        private readonly DispatcherTimer _timer = new() { Interval = TimeSpan.FromMilliseconds(32) };
        private UIElement? _root;
        private RichTextBlock? _anchorText;
        private TextPointer? _anchor;
        private Pointer? _capture;
        private Point _pressed;
        private Point _pointer;
        private nint _window;
        private bool _dragging;
        private bool _takingCapture;
        private bool _writing;
        private bool _moved;

        public string SelectedText => string.Join("\r\n\r\n", _selection.Select(range => range.Text.SelectedText).Where(text => text.Length > 0));

        public void Add(RichTextBlock text)
        {
            if (!_documents.Add(text)) return;
            text.SelectionChanged += SelectionChanged;
            if (_documents.Count != 1) return;
            _root = text.XamlRoot.Content as UIElement;
            _root?.AddHandler(UIElement.KeyDownEvent, new KeyEventHandler(KeyDown), true);
            _root?.AddHandler(UIElement.PointerPressedEvent, new PointerEventHandler(RootPressed), true);
            scroll.BringIntoViewRequested += BringIntoView;
            scroll.ViewChanged += ViewChanged;
            _timer.Tick += Tick;
        }

        public void Remove(RichTextBlock text)
        {
            Invalidate(text);
            text.SelectionChanged -= SelectionChanged;
            if (_highlights.Remove(text, out var highlight)) text.TextHighlighters.Remove(highlight);
            _documents.Remove(text);
            if (_documents.Count != 0) return;
            _root?.RemoveHandler(UIElement.KeyDownEvent, new KeyEventHandler(KeyDown));
            _root?.RemoveHandler(UIElement.PointerPressedEvent, new PointerEventHandler(RootPressed));
            scroll.BringIntoViewRequested -= BringIntoView;
            scroll.ViewChanged -= ViewChanged;
            _timer.Tick -= Tick;
            _root = null;
        }

        public void Invalidate(RichTextBlock text)
        {
            if (_anchorText == text || _selection.Any(range => range.Text == text)) { Finish(); Clear(); }
        }

        private void RootPressed(object sender, PointerRoutedEventArgs e)
        {
            if (!_dragging && e.GetCurrentPoint(null).Properties.IsLeftButtonPressed) Clear();
        }

        public void Begin(RichTextBlock text, PointerRoutedEventArgs e)
        {
            if (e.Pointer.PointerDeviceType != PointerDeviceType.Mouse || !e.GetCurrentPoint(text).Properties.IsLeftButtonPressed || _root is null) return;
            var anchor = text.GetPositionFromPoint(e.GetCurrentPoint(text).Position);
            if (anchor is null || text.Blocks.OfType<Paragraph>().Any(p => IsLinkAt(p.Inlines, anchor.Offset))) return;
            Finish();
            var initial = new Range(text, text.SelectionStart, text.SelectionEnd);
            Clear();
            _anchorText = text;
            _anchor = anchor;
            _pointer = _pressed = e.GetCurrentPoint(_root).Position;
            _window = Microsoft.UI.Win32Interop.GetWindowFromWindowId(text.XamlRoot.ContentIslandEnvironment.AppWindowId);
            _moved = false;
            _dragging = true;
            _root.AddHandler(UIElement.PointerReleasedEvent, new PointerEventHandler(Released), true);
            _root.AddHandler(UIElement.PointerCanceledEvent, new PointerEventHandler(Canceled), true);
            _root.AddHandler(UIElement.PointerCaptureLostEvent, new PointerEventHandler(Canceled), true);
            _takingCapture = true;
            try
            {
                text.ReleasePointerCapture(e.Pointer);
                // Reset native pressed state, not just capture, to prevent selecting on hover after release.
                text.IsTextSelectionEnabled = false;
                text.IsTextSelectionEnabled = true;
                text.Select(initial.Start, initial.End);
                if (_root.CapturePointer(e.Pointer)) _capture = e.Pointer;
            }
            finally { _takingCapture = false; }
            if (_capture is null) { Finish(); return; }
            _selection = initial.Start.Offset != initial.End.Offset ? [initial] : [];
            e.Handled = true;
            _timer.Start();
        }

        private static bool IsLinkAt(IEnumerable<Inline> inlines, int offset) => inlines.Any(inline =>
            inline is Hyperlink link && offset >= link.ContentStart.Offset && offset <= link.ContentEnd.Offset
            || inline is Span span && IsLinkAt(span.Inlines, offset));

        private void Released(object sender, PointerRoutedEventArgs e)
        {
            if (_capture?.PointerId == e.Pointer.PointerId) { Finish(); e.Handled = true; }
        }

        private void Canceled(object sender, PointerRoutedEventArgs e)
        {
            if (!_takingCapture && _capture?.PointerId == e.Pointer.PointerId) Finish();
        }

        private void BringIntoView(UIElement sender, BringIntoViewRequestedEventArgs e)
        { if (_dragging || _writing) e.Handled = true; }

        private void SelectionChanged(object sender, RoutedEventArgs e)
        {
            if (_writing || _dragging || (GetAsyncKeyState(1) & 0x8000) != 0 || sender is not RichTextBlock text) return;
            var range = _selection.FirstOrDefault(item => item.Text == text);
            if (range is null || (text.SelectionStart.Offset == range.Start.Offset && text.SelectionEnd.Offset == range.End.Offset)) return;
            // Native text services can process a queued hover after mouse capture ends.
            // A released conversation selection stays immutable until a new user action.
            _writing = true;
            try { text.Select(range.Start, range.End); }
            finally { _writing = false; }
        }

        private void Tick(object? sender, object e)
        {
            if (!_dragging) return;
            if ((GetAsyncKeyState(1) & 0x8000) == 0 || GetForegroundWindow() != _window || _anchorText?.XamlRoot?.IsHostVisible != true)
            { Finish(); return; }
            if (_root is null || !GetCursorPos(out var cursor) || !ScreenToClient(_window, ref cursor)) return;
            double scale = _anchorText.XamlRoot.RasterizationScale;
            _pointer = new Point(cursor.X / scale, cursor.Y / scale);
            if (!_moved && Math.Abs(_pointer.Y - _pressed.Y) + Math.Abs(_pointer.X - _pressed.X) < 5) return;
            _moved = true;
            var position = _root.TransformToVisual(scroll).TransformPoint(_pointer);
            double delta = position.Y < 40 ? -Math.Clamp((40 - position.Y) * .45, 2, 28)
                : position.Y > scroll.ViewportHeight - 40 ? Math.Clamp((position.Y - scroll.ViewportHeight + 40) * .45, 2, 28) : 0;
            if (delta != 0) scroll.ChangeView(null, Math.Clamp(scroll.VerticalOffset + delta, 0, scroll.ScrollableHeight), null, true);
            Extend();
        }

        private void ViewChanged(object? sender, ScrollViewerViewChangedEventArgs e)
        { if (_dragging && _moved) Extend(); }

        private static bool IsVisible(RichTextBlock text)
        {
            if (!text.IsLoaded || text.ActualHeight <= 0 || text.ActualWidth <= 0) return false;
            for (DependencyObject? element = text; element is not null; element = VisualTreeHelper.GetParent(element))
                if (element is UIElement ui && ui.Visibility != Visibility.Visible) return false;
            return true;
        }

        private void Extend()
        {
            if (!_dragging || _writing || _root is null || _anchorText is null || _anchor is null) return;
            var documents = _documents.Where(IsVisible).Select(text => (Text: text, Top: text.TransformToVisual(scroll).TransformPoint(new Point()).Y))
                .OrderBy(item => item.Top).ToArray();
            int anchorIndex = Array.FindIndex(documents, item => item.Text == _anchorText);
            if (anchorIndex < 0 || documents.Length == 0) { Finish(); return; }
            var point = _root.TransformToVisual(scroll).TransformPoint(_pointer);
            point.Y = Math.Clamp(point.Y, 1, Math.Max(1, scroll.ViewportHeight - 1));
            int endIndex = Array.FindIndex(documents, item => point.Y <= item.Top + item.Text.ActualHeight);
            if (endIndex < 0) endIndex = documents.Length - 1;
            var endText = documents[endIndex].Text;
            var local = scroll.TransformToVisual(endText).TransformPoint(point);
            local.X = Math.Clamp(local.X, 0, Math.Max(0, endText.ActualWidth - 1));
            var end = local.Y <= 0 ? endText.ContentStart : local.Y >= endText.ActualHeight ? endText.ContentEnd : endText.GetPositionFromPoint(local);
            if (end is null) return;
            bool forward = endIndex > anchorIndex || (endIndex == anchorIndex && end.Offset >= _anchor.Offset);
            int first = Math.Min(anchorIndex, endIndex), last = Math.Max(anchorIndex, endIndex);
            var ranges = new List<Range>();
            for (int index = first; index <= last; index++)
            {
                var text = documents[index].Text;
                var start = index == first ? (forward ? _anchor : end) : text.ContentStart;
                var finish = index == last ? (forward ? end : _anchor) : text.ContentEnd;
                ranges.Add(new Range(text, start, finish));
            }
            _writing = true;
            try
            {
                foreach (var old in _selection.Where(old => ranges.All(current => current.Text != old.Text)))
                {
                    old.Text.Select(old.Text.ContentStart, old.Text.ContentStart);
                    if (_highlights.TryGetValue(old.Text, out var highlight)) highlight.Ranges.Clear();
                }
                foreach (var range in ranges) ApplyRange(range);
                _selection = ranges;
            }
            finally { _writing = false; }
        }

        private void Finish()
        {
            if (!_dragging) return;
            _dragging = false; // Capture-lost can reenter during release.
            _timer.Stop();
            _root?.RemoveHandler(UIElement.PointerReleasedEvent, new PointerEventHandler(Released));
            _root?.RemoveHandler(UIElement.PointerCanceledEvent, new PointerEventHandler(Canceled));
            _root?.RemoveHandler(UIElement.PointerCaptureLostEvent, new PointerEventHandler(Canceled));
            var capture = _capture;
            _capture = null;
            if (capture is not null) _root?.ReleasePointerCapture(capture);
            _writing = true;
            try
            {
                foreach (var range in _selection)
                {
                    range.Text.IsTextSelectionEnabled = false;
                    range.Text.IsTextSelectionEnabled = true;
                }
                foreach (var range in _selection) ApplyRange(range);
            }
            finally { _writing = false; }
            _anchorText = null;
            _anchor = null;
        }

        private void Clear()
        {
            _writing = true;
            try
            {
                foreach (var range in _selection)
                {
                    range.Text.Select(range.Text.ContentStart, range.Text.ContentStart);
                    if (_highlights.TryGetValue(range.Text, out var highlight)) highlight.Ranges.Clear();
                }
            }
            finally { _selection.Clear(); _writing = false; }
        }

        private void ApplyRange(Range range)
        {
            var text = range.Text;
            // TextPointer counts formatting positions; TextHighlighter counts plain characters.
            // Use the native selection to translate, including paragraphs, Unicode and code spans.
            text.Select(text.ContentStart, range.Start);
            int start = text.SelectedText.Length;
            text.Select(range.Start, range.End);
            if (!_highlights.TryGetValue(text, out var highlight))
            {
                highlight = new TextHighlighter { Background = text.SelectionHighlightColor,
                    Foreground = new SolidColorBrush(Microsoft.UI.Colors.White) };
                text.TextHighlighters.Add(highlight);
                _highlights.Add(text, highlight);
            }
            highlight.Ranges.Clear();
            if (text.SelectedText.Length > 0)
                highlight.Ranges.Add(new TextRange { StartIndex = start, Length = text.SelectedText.Length });
        }

        private void KeyDown(object sender, KeyRoutedEventArgs e)
        {
            if (_root?.XamlRoot is { } xamlRoot && FocusManager.GetFocusedElement(xamlRoot) is TextBox or PasswordBox or RichEditBox) return;
            bool control = (InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Control) & CoreVirtualKeyStates.Down) != 0;
            if (e.Key == VirtualKey.A && control && _selection.Count > 0)
            {
                _writing = true;
                try
                {
                    _selection = _documents.Where(IsVisible).OrderBy(text => text.TransformToVisual(scroll).TransformPoint(new Point()).Y)
                        .Select(text => new Range(text, text.ContentStart, text.ContentEnd)).ToList();
                    foreach (var range in _selection) ApplyRange(range);
                }
                finally { _writing = false; }
                e.Handled = true;
                return;
            }
            if (e.Key != VirtualKey.C || !control) return;
            if (SelectedText.Length == 0) return;
            Copy();
            e.Handled = true;
        }

        public void Copy(RichTextBlock? fallback = null)
        {
            string text = SelectedText;
            if (text.Length == 0) text = fallback?.SelectedText ?? string.Empty;
            if (text.Length == 0) return;
            var data = new DataPackage { RequestedOperation = DataPackageOperation.Copy };
            data.SetText(text);
            Clipboard.SetContent(data);
        }
    }

    [DllImport("user32.dll")] private static extern short GetAsyncKeyState(int key);
    [DllImport("user32.dll")] private static extern nint GetForegroundWindow();
    [DllImport("user32.dll")] private static extern bool GetCursorPos(out CursorPoint point);
    [DllImport("user32.dll")] private static extern bool ScreenToClient(nint window, ref CursorPoint point);
    [StructLayout(LayoutKind.Sequential)] private struct CursorPoint { public int X; public int Y; }
}
