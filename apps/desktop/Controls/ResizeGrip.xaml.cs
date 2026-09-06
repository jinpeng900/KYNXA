using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;
using Windows.System;

namespace KYNXA_Desktop.Controls;

public enum ResizeAxis
{
    Horizontal,
    Vertical
}

public sealed class ResizeDeltaEventArgs(double delta) : EventArgs
{
    public double Delta { get; } = delta;
}

public sealed partial class ResizeGrip : UserControl
{
    private bool _dragging;
    private Point _startPoint;

    public ResizeAxis Axis
    {
        get => (ResizeAxis)GetValue(AxisProperty);
        set => SetValue(AxisProperty, value);
    }

    public static readonly DependencyProperty AxisProperty = DependencyProperty.Register(
        nameof(Axis), typeof(ResizeAxis), typeof(ResizeGrip),
        new PropertyMetadata(ResizeAxis.Horizontal, OnAxisChanged));

    public event EventHandler? DragStarted;
    public event EventHandler<ResizeDeltaEventArgs>? DragDelta;
    public event EventHandler? DragCompleted;
    public event EventHandler? ResetRequested;
    public event EventHandler? CancelRequested;

    public ResizeGrip()
    {
        InitializeComponent();
        UpdateAxisVisuals();
    }

    private static void OnAxisChanged(DependencyObject sender, DependencyPropertyChangedEventArgs args) =>
        ((ResizeGrip)sender).UpdateAxisVisuals();

    private void UpdateAxisVisuals()
    {
        if (Indicator is null)
        {
            return;
        }

        if (Axis == ResizeAxis.Horizontal)
        {
            Width = 8;
            Indicator.Width = 1;
            Indicator.Height = double.NaN;
        }
        else
        {
            Height = 8;
            Indicator.Height = 1;
            Indicator.Width = double.NaN;
        }
    }

    private UIElement CoordinateRoot => XamlRoot?.Content as UIElement ?? this;

    private void Grip_PointerPressed(object sender, PointerRoutedEventArgs e)
    {
        _dragging = CapturePointer(e.Pointer);
        if (!_dragging)
        {
            return;
        }

        _startPoint = e.GetCurrentPoint(CoordinateRoot).Position;
        Focus(FocusState.Pointer);
        DragStarted?.Invoke(this, EventArgs.Empty);
        e.Handled = true;
    }

    private void Grip_PointerMoved(object sender, PointerRoutedEventArgs e)
    {
        if (!_dragging)
        {
            return;
        }

        Point current = e.GetCurrentPoint(CoordinateRoot).Position;
        double delta = Axis == ResizeAxis.Horizontal ? current.X - _startPoint.X : current.Y - _startPoint.Y;
        DragDelta?.Invoke(this, new ResizeDeltaEventArgs(delta));
        e.Handled = true;
    }

    private void Grip_PointerReleased(object sender, PointerRoutedEventArgs e)
    {
        if (!_dragging)
        {
            return;
        }

        _dragging = false;
        ReleasePointerCapture(e.Pointer);
        DragCompleted?.Invoke(this, EventArgs.Empty);
        e.Handled = true;
    }

    private void Grip_DoubleTapped(object sender, DoubleTappedRoutedEventArgs e)
    {
        ResetRequested?.Invoke(this, EventArgs.Empty);
        e.Handled = true;
    }

    private void Grip_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (!_dragging || e.Key != VirtualKey.Escape)
        {
            return;
        }

        _dragging = false;
        ReleasePointerCaptures();
        CancelRequested?.Invoke(this, EventArgs.Empty);
        e.Handled = true;
    }

    private void Grip_PointerEntered(object sender, PointerRoutedEventArgs e)
    {
        Indicator.Background = (Brush)Application.Current.Resources["KynxaDividerBrush"];
        ProtectedCursor = InputSystemCursor.Create(
            Axis == ResizeAxis.Horizontal ? InputSystemCursorShape.SizeWestEast : InputSystemCursorShape.SizeNorthSouth);
    }

    private void Grip_PointerExited(object sender, PointerRoutedEventArgs e)
    {
        if (!_dragging)
        {
            Indicator.Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        }
    }
}
