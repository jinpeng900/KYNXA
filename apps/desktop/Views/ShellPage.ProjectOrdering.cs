using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;
using System.Runtime.InteropServices;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private readonly HashSet<Guid> _collapsedByUser = [];
    private bool _renderingProjects;
    private Grid? _orderingRow;
    private Grid? _orderingTarget;
    private Point _orderingStart;
    private bool _orderingMoved;
    private bool _orderingAfter;
    private readonly HashSet<Grid> _orderRows = [];

    private void ProjectRow_OrderLoaded(object sender, RoutedEventArgs args)
    {
        if (sender is not Grid row) return;
        _orderRows.Add(row);
        row.RemoveHandler(UIElement.PointerMovedEvent, new PointerEventHandler(ProjectRow_OrderMoved));
        row.AddHandler(UIElement.PointerMovedEvent, new PointerEventHandler(ProjectRow_OrderMoved), true);
    }

    private void ProjectRow_OrderUnloaded(object sender, RoutedEventArgs args)
    {
        if (sender is Grid row) _orderRows.Remove(row);
    }

    private void ProjectTree_Collapsed(TreeView sender, TreeViewCollapsedEventArgs args)
    {
        if (!_renderingProjects && args.Node.Content is ProjectTreeEntry { Chat: null } entry)
            _collapsedByUser.Add(entry.Project.Id);
    }

    private void ProjectTree_Expanding(TreeView sender, TreeViewExpandingEventArgs args)
    {
        if (!_renderingProjects && args.Node.Content is ProjectTreeEntry { Chat: null } entry)
            _collapsedByUser.Remove(entry.Project.Id);
    }

    private void ProjectRow_OrderPressed(object sender, PointerRoutedEventArgs args)
    {
        if (sender is not Grid { Tag: ProjectTreeEntry } row
            || args.Pointer.PointerDeviceType != Microsoft.UI.Input.PointerDeviceType.Mouse
            || !args.GetCurrentPoint(row).Properties.IsLeftButtonPressed) return;
        for (DependencyObject? child = args.OriginalSource as DependencyObject; child is not null && child != row;
             child = VisualTreeHelper.GetParent(child))
            if (child is Button) return;
        _orderingStart = ProjectPointerPosition(args);
        _orderingMoved = false;
        if (row.CapturePointer(args.Pointer)) { _orderingRow = row; args.Handled = true; }
    }

    private void ProjectRow_OrderMoved(object sender, PointerRoutedEventArgs args)
    {
        if (_orderingRow is not { Tag: ProjectTreeEntry source } row) return;
        if (!args.GetCurrentPoint(row).Properties.IsLeftButtonPressed) { ClearProjectOrdering(); return; }
        var point = ProjectPointerPosition(args);
        if (!_orderingMoved && Math.Abs(point.Y - _orderingStart.Y) + Math.Abs(point.X - _orderingStart.X) < 6) return;
        _orderingMoved = true;
        args.Handled = true;
        UpdateProjectOrderTarget(point, source);
    }

    private void UpdateProjectOrderTarget(Point point, ProjectTreeEntry source)
    {
        if (_orderingTarget is not null) _orderingTarget.BorderThickness = new Thickness(0);
        _orderingTarget = null;
        if (source.Chat is not null) return;
        foreach (var targetRow in _orderRows)
        {
            if (!targetRow.IsLoaded || targetRow.Tag is not ProjectTreeEntry target
                || target.Project == source.Project || target.Project.IsPinned != source.Project.IsPinned) continue;
            var origin = targetRow.TransformToVisual(ProjectTree).TransformPoint(new Point());
            if (!new Rect(origin, new Size(targetRow.ActualWidth, targetRow.ActualHeight)).Contains(point)) continue;
            _orderingTarget = targetRow;
            _orderingAfter = point.Y - origin.Y > targetRow.ActualHeight / 2;
            targetRow.BorderBrush = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 145, 145, 145));
            targetRow.BorderThickness = new Thickness(0, _orderingAfter ? 0 : 1, 0, _orderingAfter ? 1 : 0);
            break;
        }
    }

    private async void ProjectRow_OrderReleased(object sender, PointerRoutedEventArgs args)
    {
        if (_orderingRow is not { Tag: ProjectTreeEntry source } row) return;
        if (_orderingMoved) UpdateProjectOrderTarget(ProjectPointerPosition(args), source);
        var target = _orderingTarget?.Tag as ProjectTreeEntry;
        bool moved = _orderingMoved, after = _orderingAfter;
        ClearProjectOrdering();
        row.ReleasePointerCapture(args.Pointer);
        args.Handled = true;
        if (!moved)
        {
            if (source.Chat is null) source.IsExpanded = !source.IsExpanded;
            else SelectProjectChat(source.Project, source.Chat);
        }
        else if (source.Chat is null && target is not null)
            await RunProjectActionAsync(() =>
            {
                if (ProjectOrdering.Move(_projects, source.Project, target.Project, after)) SaveProjectsAndRender();
                return Task.CompletedTask;
            });
    }

    private void ProjectRow_OrderCancelled(object sender, PointerRoutedEventArgs args) => ClearProjectOrdering();

    private void ClearProjectOrdering()
    {
        if (_orderingTarget is not null) _orderingTarget.BorderThickness = new Thickness(0);
        _orderingRow = _orderingTarget = null;
        _orderingMoved = false;
    }

    private Point ProjectPointerPosition(PointerRoutedEventArgs args)
    {
        if (ReadProjectCursor(out var cursor) && ProjectScreenToClient(WinRT.Interop.WindowNative.GetWindowHandle(App.Window), ref cursor)
            && XamlRoot.Content is UIElement root)
            return root.TransformToVisual(ProjectTree).TransformPoint(new Point(cursor.X / XamlRoot.RasterizationScale,
                cursor.Y / XamlRoot.RasterizationScale));
        return args.GetCurrentPoint(ProjectTree).Position;
    }

    [StructLayout(LayoutKind.Sequential)] private struct ProjectCursorPoint { public int X; public int Y; }
    [DllImport("user32.dll", EntryPoint = "GetCursorPos")] private static extern bool ReadProjectCursor(out ProjectCursorPoint point);
    [DllImport("user32.dll", EntryPoint = "ScreenToClient")] private static extern bool ProjectScreenToClient(nint window, ref ProjectCursorPoint point);
}
