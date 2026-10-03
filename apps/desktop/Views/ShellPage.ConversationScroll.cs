using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.System;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private ScrollViewer? _conversationScrollViewer;
    private readonly Dictionary<Guid, (double Offset, bool Follow)> _conversationScrollPositions = [];
    private bool _followLatest = true;
    private bool _latestScrollPending;
    private bool _hasUnreadReply;
    private double _lastConversationOffset;
    private DispatcherTimer? _conversationRestoreTimer;
    private Guid? _conversationRestoreChatId;
    private double? _conversationRestoreOffset;
    private bool _conversationRestoreFollow;
    private bool _conversationScrollSettling;
    private int _conversationScrollGeneration;
    private int _conversationRestoreAttempts;
    private int _conversationRestoreStablePasses;
    private double _conversationRestoreLastExtent;
    private const int ConversationRestoreMaximumAttempts = 20;

    private void ConversationMessages_SizeChanged(object sender, SizeChangedEventArgs args)
    {
        ConversationMessages_Loaded(sender, args);
        UpdateJumpToLatest();
    }

    private void ConversationMessages_Loaded(object sender, RoutedEventArgs args)
    {
        if (_conversationScrollViewer is not null) return;
        _conversationScrollViewer = FindConversationScrollViewer(ConversationMessages);
        if (_conversationScrollViewer is null) return;
        _conversationScrollViewer.ViewChanged += ConversationScroll_ViewChanged;
        _conversationScrollViewer.AddHandler(UIElement.PointerPressedEvent, new PointerEventHandler(ConversationScroll_PointerPressed), true);
        _conversationScrollViewer.AddHandler(UIElement.PointerWheelChangedEvent, new PointerEventHandler(ConversationScroll_PointerWheelChanged), true);
        _conversationScrollViewer.AddHandler(UIElement.KeyDownEvent, new KeyEventHandler(ConversationScroll_KeyDown), true);
        UpdateJumpToLatest();
    }

    private static ScrollViewer? FindConversationScrollViewer(DependencyObject element)
    {
        if (element is ScrollViewer scroll) return scroll;
        for (int index = 0; index < VisualTreeHelper.GetChildrenCount(element); index++)
            if (FindConversationScrollViewer(VisualTreeHelper.GetChild(element, index)) is { } result) return result;
        return null;
    }

    private void ConversationMessages_Unloaded(object sender, RoutedEventArgs args)
    {
        CancelConversationScrollRestore();
        if (_conversationRestoreTimer is not null)
        {
            _conversationRestoreTimer.Tick -= ConversationScrollRestore_Tick;
            _conversationRestoreTimer = null;
        }
        if (_conversationScrollViewer is null) return;
        _conversationScrollViewer.ViewChanged -= ConversationScroll_ViewChanged;
        _conversationScrollViewer.RemoveHandler(UIElement.PointerPressedEvent, new PointerEventHandler(ConversationScroll_PointerPressed));
        _conversationScrollViewer.RemoveHandler(UIElement.PointerWheelChangedEvent, new PointerEventHandler(ConversationScroll_PointerWheelChanged));
        _conversationScrollViewer.RemoveHandler(UIElement.KeyDownEvent, new KeyEventHandler(ConversationScroll_KeyDown));
        _conversationScrollViewer = null;
    }

    private bool ConversationIsAtBottom() => _conversationScrollViewer is null ||
        _conversationScrollViewer.ScrollableHeight - _conversationScrollViewer.VerticalOffset < 80;

    private void ConversationScroll_ViewChanged(object? sender, ScrollViewerViewChangedEventArgs args)
    {
        if (_conversationScrollViewer is null) return;
        double offset = _conversationScrollViewer.VerticalOffset;
        // A larger content extent alone must not change the user's reading preference.
        if (!_latestScrollPending && Math.Abs(offset - _lastConversationOffset) > .5)
        {
            _followLatest = ConversationIsAtBottom();
            if (_followLatest) _hasUnreadReply = false;
        }
        _lastConversationOffset = offset;
        if (_latestScrollPending && !_conversationScrollSettling && !args.IsIntermediate) _latestScrollPending = false;
        UpdateJumpToLatest();
    }

    private void ConversationScroll_PointerPressed(object sender, PointerRoutedEventArgs args)
    {
        for (DependencyObject? child = args.OriginalSource as DependencyObject; child is not null && child != _conversationScrollViewer;
             child = VisualTreeHelper.GetParent(child))
            if (child is Button) return;
        CancelConversationScrollRestore();
        _followLatest = false;
    }

    private void ConversationScroll_PointerWheelChanged(object sender, PointerRoutedEventArgs args)
    {
        CancelConversationScrollRestore();
        _followLatest = args.GetCurrentPoint(_conversationScrollViewer).Properties.MouseWheelDelta <= 0 && ConversationIsAtBottom();
    }

    private void ConversationScroll_KeyDown(object sender, KeyRoutedEventArgs args)
    {
        if (args.Key is not (VirtualKey.Up or VirtualKey.Down or VirtualKey.PageUp or VirtualKey.PageDown or VirtualKey.Home or VirtualKey.End)) return;
        CancelConversationScrollRestore();
        _followLatest = false;
    }

    private (bool Follow, double? RestoreOffset) PrepareConversationScroll(Guid? conversationId)
    {
        bool changed = _presentedChatId != conversationId;
        double? restore = null;
        if (changed)
        {
            if (_presentedChatId is Guid previous && _conversationScrollViewer is not null)
                _conversationScrollPositions[previous] =
                    (_conversationScrollSettling && _conversationRestoreChatId == previous && !_conversationRestoreFollow
                        ? _conversationRestoreOffset ?? _conversationScrollViewer.VerticalOffset : _conversationScrollViewer.VerticalOffset,
                    _followLatest);
            CancelConversationScrollRestore();
            if (conversationId is Guid next && _conversationScrollPositions.TryGetValue(next, out var position))
            { _followLatest = position.Follow; if (!position.Follow) restore = position.Offset; }
            else _followLatest = true;
            _hasUnreadReply = false;
        }
        else if (_conversationScrollSettling && _conversationRestoreChatId == conversationId && !_conversationRestoreFollow)
            restore = _conversationRestoreOffset;
        _latestScrollPending = _followLatest || restore is not null;
        return (_followLatest, restore);
    }

    private void SynchronizeConversationMessages(List<ConversationMessageViewModel> desired)
    {
        // Keep the existing text controls and selection when only a new reply/status is appended.
        int common = 0;
        while (common < ActiveMessages.Count && common < desired.Count && ReferenceEquals(ActiveMessages[common], desired[common])) common++;
        while (ActiveMessages.Count > common) ActiveMessages.RemoveAt(ActiveMessages.Count - 1);
        for (int index = common; index < desired.Count; index++) ActiveMessages.Add(desired[index]);
    }

    private void FinishConversationScroll(Guid? conversationId, bool follow, double? restore)
    {
        CancelConversationScrollRestore();
        if (ActiveMessages.Count == 0) { _latestScrollPending = false; UpdateJumpToLatest(); return; }
        bool requestedFollow = follow && _followLatest;
        if (!requestedFollow && restore is null) { UpdateJumpToLatest(); return; }
        int generation = _conversationScrollGeneration;
        _conversationRestoreChatId = conversationId;
        _conversationRestoreFollow = requestedFollow;
        _conversationRestoreOffset = restore;
        _conversationRestoreAttempts = 0;
        _conversationRestoreStablePasses = 0;
        _conversationRestoreLastExtent = double.NaN;
        _conversationScrollSettling = _latestScrollPending = true;
        DispatcherQueue.TryEnqueue(() =>
        {
            if (_chatClosing || generation != _conversationScrollGeneration || CurrentChat?.Id != conversationId) return;
            if (_conversationRestoreTimer is null)
            {
                _conversationRestoreTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(16) };
                _conversationRestoreTimer.Tick += ConversationScrollRestore_Tick;
            }
            _conversationRestoreTimer.Start();
            ConversationScrollRestore_Tick(null, new object());
        });
    }

    private void CancelConversationScrollRestore()
    {
        _conversationScrollGeneration++;
        _conversationRestoreTimer?.Stop();
        _conversationScrollSettling = _latestScrollPending = false;
        _conversationRestoreChatId = null;
        _conversationRestoreOffset = null;
    }

    private void ConversationScrollRestore_Tick(object? sender, object args)
    {
        if (_chatClosing || !_conversationScrollSettling || CurrentChat?.Id != _conversationRestoreChatId)
        { CancelConversationScrollRestore(); return; }
        _conversationRestoreAttempts++;
        // x:Bind and Markdown controls can change the extent after the first queued layout.
        // Keep the original offset until layout and the native scroll position both settle.
        ConversationMessages.UpdateLayout();
        ConversationMessages_Loaded(ConversationMessages, new RoutedEventArgs());
        if (_conversationScrollViewer is { } scroll)
        {
            double extent = scroll.ScrollableHeight;
            double requested = _conversationRestoreFollow ? extent : _conversationRestoreOffset ?? scroll.VerticalOffset;
            double target = Math.Clamp(requested, 0, extent);
            bool reached = Math.Abs(scroll.VerticalOffset - target) < .5;
            bool settled = reached && extent + .5 >= requested && Math.Abs(extent - _conversationRestoreLastExtent) < .5;
            _conversationRestoreStablePasses = settled ? _conversationRestoreStablePasses + 1 : 0;
            _conversationRestoreLastExtent = extent;
            if (!reached) scroll.ChangeView(null, target, null, true);
            _lastConversationOffset = scroll.VerticalOffset;
        }
        if (_conversationRestoreStablePasses >= 3 || _conversationRestoreAttempts >= ConversationRestoreMaximumAttempts)
            CancelConversationScrollRestore();
        UpdateJumpToLatest();
    }

    private void UpdateJumpToLatest()
    {
        if (JumpToLatestButton is null) return;
        JumpToLatestButton.Visibility = ActiveMessages.Count > 0 && !ConversationIsAtBottom() ? Visibility.Visible : Visibility.Collapsed;
        JumpToLatestButton.Content = _hasUnreadReply ? "有新回复 · 回到最新 ↓" : "回到最新 ↓";
    }

    private void JumpToLatest_Click(object sender, RoutedEventArgs args)
    {
        _followLatest = true;
        _hasUnreadReply = false;
        _latestScrollPending = true;
        FinishConversationScroll(CurrentChat?.Id, true, null);
    }
}
