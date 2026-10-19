using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Foundation;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private bool _approvalLayoutTracking;
    private bool _approvalQueueMasked;
    private double _approvalQueueOpacity;
    private bool _approvalQueueHitTest;
    private bool _approvalQueueEnabled;
    private bool _positioningApproval;
    private double _approvalComposerHeightLimit = double.PositiveInfinity;

    private void InitializeInlineApprovals()
    {
        InlineApprovalCard.DecisionRequested += InlineApprovalDecisionRequested;
        InlineApprovalCard.SizeChanged += (_, _) => PositionInlineApproval();
        Loaded += (_, _) => RefreshInlineApproval();
        Unloaded += (_, _) => HideInlineApproval();
    }

    private void ShowInlineApproval()
    {
        ApprovalOverlay.Visibility = Visibility.Visible;
        if (!_approvalQueueMasked)
        {
            _approvalQueueOpacity = PromptQueue.Opacity;
            _approvalQueueHitTest = PromptQueue.IsHitTestVisible;
            _approvalQueueEnabled = PromptQueue.IsEnabled;
            _approvalQueueMasked = true;
            // Preserve queue height and editor state while excluding covered controls from interaction.
            // 保留队列高度与编辑状态，同时禁止操作被遮盖的控件。
            PromptQueue.Opacity = 0;
            PromptQueue.IsHitTestVisible = false;
            PromptQueue.IsEnabled = false;
        }
        if (!_approvalLayoutTracking)
        {
            _approvalLayoutTracking = true;
            LayoutUpdated += InlineApprovalLayoutUpdated;
        }
        PositionInlineApproval();
    }

    private void HideInlineApproval()
    {
        ApprovalOverlay.Visibility = Visibility.Collapsed;
        _visibleApprovalId = null;
        if (_approvalLayoutTracking)
        {
            _approvalLayoutTracking = false;
            LayoutUpdated -= InlineApprovalLayoutUpdated;
        }
        if (double.IsFinite(_approvalComposerHeightLimit))
        {
            _approvalComposerHeightLimit = double.PositiveInfinity;
            if (IsLoaded) ApplyLayout();
        }
        if (!_approvalQueueMasked) return;
        PromptQueue.Opacity = _approvalQueueOpacity;
        PromptQueue.IsHitTestVisible = _approvalQueueHitTest;
        PromptQueue.IsEnabled = _approvalQueueEnabled;
        _approvalQueueMasked = false;
    }

    private void InlineApprovalLayoutUpdated(object? sender, object args) => PositionInlineApproval();

    private void PositionInlineApproval()
    {
        if (_positioningApproval || _applyingLayout || !_approvalLayoutTracking || !IsLoaded || ComposerHost.ActualWidth <= 0 || ApprovalOverlay.ActualWidth <= 0) return;
        _positioningApproval = true;
        try { PositionInlineApprovalCore(); }
        finally { _positioningApproval = false; }
    }

    private void PositionInlineApprovalCore()
    {
        var anchor = ComposerHost.TransformToVisual(ApprovalOverlay).TransformPoint(new Point());
        double inset = ComposerHost.ActualWidth < 420 ? 12 : 24;
        double width = Math.Max(1, Math.Min(ComposerHost.ActualWidth - inset * 2, ApprovalOverlay.ActualWidth - 16));
        if (Math.Abs(InlineApprovalCard.Width - width) > .5 || double.IsNaN(InlineApprovalCard.Width)) InlineApprovalCard.Width = width;
        double availableHeight = Math.Max(200, anchor.Y - 16);
        if (Math.Abs(InlineApprovalCard.MaxHeight - availableHeight) > .5) InlineApprovalCard.MaxHeight = availableHeight;
        double left = Math.Clamp(anchor.X + (ComposerHost.ActualWidth - width) / 2, 8, Math.Max(8, ApprovalOverlay.ActualWidth - width - 8));
        double top = Math.Max(8, anchor.Y - InlineApprovalCard.ActualHeight - 8);
        if (Math.Abs(Canvas.GetLeft(InlineApprovalCard) - left) > .5 || double.IsNaN(Canvas.GetLeft(InlineApprovalCard))) Canvas.SetLeft(InlineApprovalCard, left);
        if (Math.Abs(Canvas.GetTop(InlineApprovalCard) - top) > .5 || double.IsNaN(Canvas.GetTop(InlineApprovalCard))) Canvas.SetTop(InlineApprovalCard, top);
        if (InlineApprovalCard.ActualHeight <= 0) return;
        // Reserve only the vertical space a short viewport actually needs; never persist this temporary editor limit.
        // 仅在矮窗口需要时临时给审批留出垂直空间，不修改用户保存的输入区高度或展开偏好。
        double limit = Math.Max(72, ComposerHost.EditorHeight + anchor.Y - InlineApprovalCard.ActualHeight - 16);
        if (!double.IsFinite(_approvalComposerHeightLimit) || Math.Abs(limit - _approvalComposerHeightLimit) > 1)
        {
            _approvalComposerHeightLimit = limit;
            ApplyLayout();
        }
    }
}
