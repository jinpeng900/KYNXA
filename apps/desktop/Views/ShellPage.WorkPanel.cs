using KYNXA_Desktop.Controls;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using KYNXA_Desktop.Layout;
using static KYNXA_Desktop.Layout.ShellLayoutMetrics;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private double _workPanelWidthBeforeDrag;

    private double UpdateWorkPanelLayout()
    {
        double available = MainRegion.ActualWidth;
        bool canOpen = !ViewModel.IsChatMode && _activeProjectChat is not null;
        bool hasSpace = CanShowWorkPanel(available, MainRegion.ActualHeight);
        bool visible = canOpen && _layout.PreviewVisible;
        bool overlay = visible && !hasSpace;
        double panelWidth = visible ? (overlay ? GetWorkPanelOverlayWidth(available) : GetWorkPanelWidth(available, _layout.PreviewWidth)) : 0;
        double gap = visible && !overlay ? WorkPanelGap : 0;

        PreviewColumn.Width = new GridLength(overlay ? 0 : panelWidth);
        PreviewGripColumn.Width = new GridLength(gap);
        Grid.SetColumn(WorkContextPanel, overlay ? 0 : 2);
        Grid.SetColumnSpan(WorkContextPanel, overlay ? 3 : 1);
        WorkContextPanel.HorizontalAlignment = overlay ? HorizontalAlignment.Right : HorizontalAlignment.Stretch;
        WorkContextPanel.Width = overlay ? panelWidth : double.NaN;
        WorkContextPanel.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
        WorkPanelGrip.Visibility = visible && !overlay ? Visibility.Visible : Visibility.Collapsed;
        OpenWorkPanelButton.Visibility = canOpen && !visible ? Visibility.Visible : Visibility.Collapsed;
        ConversationTitle.Margin = new Thickness(24, 16, 96, 0);
        UpdateWorkDetailPresentation();

        // Automatic hiding never changes the user's explicit open/closed preference.
        return Math.Max(0, available - (overlay ? 0 : panelWidth) - gap);
    }

    private double GetWorkPanelMaximumWidth() => ShellLayoutMetrics.GetWorkPanelMaximumWidth(MainRegion.ActualWidth);

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
