using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private readonly HashSet<Grid> _hoveredProjectRows = [];
    private Grid? _projectMenuRow;
    private bool _projectHeaderHovered;
    private bool _projectHeaderMenuOpen;

    private static Grid? FindProjectRow(DependencyObject? child)
    {
        while (child is not null)
        {
            if (child is Grid row && row.Tag is ProjectTreeEntry or RecentConversation) return row;
            child = VisualTreeHelper.GetParent(child);
        }
        return null;
    }

    private bool ContainsKeyboardFocus(DependencyObject parent)
    {
        if (_projectViewClosed || parent is not FrameworkElement { IsLoaded: true, XamlRoot: { Content: not null } root })
            return false;
        DependencyObject? child;
        try { child = FocusManager.GetFocusedElement(root) as DependencyObject; }
        catch (ArgumentException)
        {
            // PointerExited/LostFocus can arrive while XAML detaches this root.
            // XAML 移除当前根节点时，PointerExited 或 LostFocus 仍可能到达。
            return false;
        }
        if (child is Control control && control.FocusState != FocusState.Keyboard) return false;
        while (child is not null)
        {
            if (child == parent) return true;
            child = VisualTreeHelper.GetParent(child);
        }
        return false;
    }

    private void UpdateProjectRowActions(Grid row)
    {
        if (_projectViewClosed || !row.IsLoaded || row.Tag is not (ProjectTreeEntry or RecentConversation)) return;
        bool show = _hoveredProjectRows.Contains(row) || row == _projectMenuRow || ContainsKeyboardFocus(row);
        if (row.FindName("ProjectRowActions") is StackPanel actions)
        {
            actions.Opacity = show ? 1 : 0;
            actions.IsHitTestVisible = show;
        }
        double reservedWidth = row.Tag is ProjectTreeEntry { Chat: null } ? 56 : 28;
        if (row.FindName("ProjectRowTitle") is TextBlock title) title.Margin = new Thickness(0, 0, show ? reservedWidth : 0, 0);
    }

    private void ProjectRow_PointerEntered(object sender, PointerRoutedEventArgs e)
    {
        if (!_projectViewClosed && sender is Grid { IsLoaded: true } row) { _hoveredProjectRows.Add(row); UpdateProjectRowActions(row); }
    }

    private void ProjectRow_PointerExited(object sender, PointerRoutedEventArgs e)
    {
        if (sender is Grid row) { _hoveredProjectRows.Remove(row); UpdateProjectRowActions(row); }
    }

    private void ProjectRow_GotFocus(object sender, RoutedEventArgs e)
    {
        if (sender is Grid row) UpdateProjectRowActions(row);
    }

    private void ProjectRow_LostFocus(object sender, RoutedEventArgs e)
    {
        if (sender is Grid row) DispatcherQueue.TryEnqueue(() => UpdateProjectRowActions(row));
    }

    private void ProjectRow_Unloaded(object sender, RoutedEventArgs e)
    {
        if (sender is not Grid row) return;
        _hoveredProjectRows.Remove(row);
        if (_projectMenuRow == row) _projectMenuRow = null;
    }

    private void UpdateProjectHeaderActions()
    {
        if (_projectViewClosed || !ProjectsHeader.IsLoaded) return;
        bool show = _projectHeaderHovered || _projectHeaderMenuOpen || ContainsKeyboardFocus(ProjectsHeader);
        AddProjectButton.Opacity = show ? 1 : 0;
        AddProjectButton.IsHitTestVisible = show;
    }

    private void ProjectsHeader_PointerEntered(object sender, PointerRoutedEventArgs e)
    { _projectHeaderHovered = true; UpdateProjectHeaderActions(); }
    private void ProjectsHeader_PointerExited(object sender, PointerRoutedEventArgs e)
    { _projectHeaderHovered = false; UpdateProjectHeaderActions(); }
    private void ProjectsHeader_GotFocus(object sender, RoutedEventArgs e) => UpdateProjectHeaderActions();
    private void ProjectsHeader_LostFocus(object sender, RoutedEventArgs e) => DispatcherQueue.TryEnqueue(UpdateProjectHeaderActions);
    private void AddProjectMenu_Opened(object sender, object e)
    { _projectHeaderMenuOpen = true; UpdateProjectHeaderActions(); }
    private void AddProjectMenu_Closed(object sender, object e)
    { _projectHeaderMenuOpen = false; UpdateProjectHeaderActions(); }
}
