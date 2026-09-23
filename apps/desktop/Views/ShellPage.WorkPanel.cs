using KYNXA_Desktop.Controls;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private const double WorkPanelMinimumWidth = 320;
    private const double WorkPanelMaximumWidth = 1600;
    private const double WorkPanelGap = 10;
    private const double WorkChatMinimumWidth = 420;
    private const double WorkPanelAutomaticThreshold = 900;
    private double _workPanelWidthBeforeDrag;

    private double UpdateWorkPanelLayout()
    {
        double available = MainRegion.ActualWidth;
        bool isWorkConversation = !ViewModel.IsChatMode && _activeProjectChat is not null && ActiveMessages.Count > 0;
        bool hasSpace = available >= WorkPanelAutomaticThreshold && MainRegion.ActualHeight >= 440;
        bool canOpen = isWorkConversation && hasSpace;
        bool visible = canOpen && _layout.PreviewVisible;
        double requestedWidth = double.IsFinite(_layout.PreviewWidth) && _layout.PreviewWidth >= WorkPanelMinimumWidth
            ? _layout.PreviewWidth : Math.Clamp(available * 0.3, WorkPanelMinimumWidth, 576);
        double panelWidth = visible ? Math.Clamp(requestedWidth, WorkPanelMinimumWidth, GetWorkPanelMaximumWidth()) : 0;
        double gap = visible ? WorkPanelGap : 0;

        PreviewColumn.Width = new GridLength(panelWidth);
        PreviewGripColumn.Width = new GridLength(gap);
        WorkContextPanel.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
        WorkPanelGrip.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
        OpenWorkPanelButton.Visibility = canOpen && !visible ? Visibility.Visible : Visibility.Collapsed;
        ConversationTitle.Margin = new Thickness(24, 16, canOpen && !visible ? 56 : 24, 0);

        // Automatic hiding never changes the user's explicit open/closed preference.
        return Math.Max(0, available - panelWidth - gap);
    }

    private double GetWorkPanelMaximumWidth() => Math.Max(WorkPanelMinimumWidth,
        Math.Min(WorkPanelMaximumWidth, MainRegion.ActualWidth - WorkChatMinimumWidth - WorkPanelGap));

    private void WorkPanelGrip_DragStarted(object? sender, EventArgs e)
    {
        _workPanelWidthBeforeDrag = _layout.PreviewWidth;
        _dragStartValue = PreviewColumn.ActualWidth;
    }

    private void WorkPanelGrip_DragDelta(object? sender, ResizeDeltaEventArgs e)
    {
        _layout.PreviewWidth = Math.Clamp(_dragStartValue - e.Delta, WorkPanelMinimumWidth, GetWorkPanelMaximumWidth());
        ApplyLayout();
    }

    private void WorkPanelGrip_CancelRequested(object? sender, EventArgs e)
    {
        _layout.PreviewWidth = _workPanelWidthBeforeDrag;
        ApplyLayout();
    }

    private void WorkPanelGrip_ResetRequested(object? sender, EventArgs e)
    {
        _layout.PreviewWidth = 0;
        ApplyLayout();
        SaveLayout();
    }

    private void OpenWorkPanelButton_Click(object sender, RoutedEventArgs e)
    {
        _layout.PreviewVisible = true;
        ApplyLayout();
        SaveLayout();
    }

    private void CloseWorkPanelButton_Click(object sender, RoutedEventArgs e)
    {
        _layout.PreviewVisible = false;
        ApplyLayout();
        SaveLayout();
    }
}
