using System.Collections.ObjectModel;
using System.Runtime.InteropServices;
using System.Text.Json;
using KYNXA_Desktop.Layout;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.ViewModels;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;

namespace KYNXA_Desktop;

public partial class App
{
    private async Task CheckSidebarRoundnessAsync()
    {
        if (ActiveId is null) await SearchAndSelectAsync(_gateway.WorkChatId, "Work fixture");
        var layout = ShellField<LayoutState>("_layout");
        var originalRecent = _shell.WorkRecentEntries.ToArray();
        var originalTasks = _shell.WorkTaskEntries.ToArray();
        var originalProjects = _shell.ProjectEntries.ToArray();
        var originalHistory = _shell.ViewModel.RecentConversations.ToArray();
        var history = Element<ListView>("ChatHistoryList");
        object? originalSelection = history.SelectedItem;
        var workContent = Element<Grid>("WorkSidebarContent");
        var chatContent = Element<Grid>("ChatSidebarContent");
        var tree = Element<TreeView>("ProjectTree");
        var originalWorkVisibility = workContent.Visibility;
        var originalChatVisibility = chatContent.Visibility;
        var originalTreeVisibility = tree.Visibility;
        var originalWindowSize = Window.AppWindow.Size;
        var originalWindowPosition = Window.AppWindow.Position;
        nint windowHandle = WinRT.Interop.WindowNative.GetWindowHandle(Window);
        if (!GetCursorPos(out NativePoint originalPointer)) throw new InvalidOperationException("Cannot read the original pointer position.");
        string originalDraft = Prompt.Text;
        var originalRows = _shell.ActiveMessages.ToArray();
        Guid? originalChat = ActiveId;
        double originalWidth = layout.SidebarWidth;
        bool originalCollapsed = layout.SidebarCollapsed;
        bool originalRecentExpanded = layout.WorkRecentExpanded;
        double originalNavigationRatio = layout.WorkNavigationRatio;
        double originalRecentRatio = layout.WorkRecentRatio;
        bool originalRenderingProjects = ShellField<bool>("_renderingProjects");
        string FormalState() => JsonSerializer.Serialize(new
        {
            projects = ShellField<List<ProjectState>>("_projects"),
            standalone = ShellField<List<ProjectChatState>>("_standaloneChats"),
            catalog = _gateway.Catalog,
            messages = _shell.ActiveMessages.Select(row => row.Message).ToArray()
        });
        const string draft = "SIDEBAR_ROUNDNESS_UNSENT_DRAFT 保留草稿";
        var observations = new Dictionary<string, object>();
        var syntheticProject = new ProjectState { Name = "Roundness synthetic project" };
        ProjectTreeEntry WorkRow(string prefix, int index) => new(syntheticProject,
            new ProjectChatState { Title = $"{prefix}_{index:00} 中文长标题 " + new string('W', 90) });
        var recentRows = Enumerable.Range(0, 30).Select(index => WorkRow("RECENT_ROUNDNESS", index)).ToArray();
        var taskRows = Enumerable.Range(0, 30).Select(index => WorkRow("TASK_ROUNDNESS", index)).ToArray();
        var historyRows = Enumerable.Range(0, 30).Select(index =>
            new RecentConversation($"HISTORY_ROUNDNESS_{index:00} 中文长标题 " + new string('H', 90), "synthetic")).ToArray();
        var parent = new ProjectTreeEntry(syntheticProject) { IsExpanded = true };
        var child = WorkRow("TREE_CHILD_ROUNDNESS", 0);
        parent.Children.Add(child);
        var treeRows = new[] { parent }.Concat(Enumerable.Range(1, 12).Select(index =>
            new ProjectTreeEntry(new ProjectState { Name = "TREE_PARENT_ROUNDNESS_" + index }))).ToArray();
        try
        {
            Window.Activate();
            SetForegroundWindow(windowHandle);
            await WaitAsync(() => GetForegroundWindow() == windowHandle,
                "only the fixture's owned window is foreground before native sidebar hover input");
            NativeUi.SetText(Prompt, draft);
            await SettleAsync();
            string formalBefore = FormalState();
            // Synthetic rows belong only to the bound view collections, never to the formal catalog.
            // 合成条目仅进入绑定的视图集合，不加入正式目录；替换期间抑制树展开保存回调。
            SetShellField("_renderingProjects", true);
            ReplaceSidebarRows(_shell.WorkRecentEntries, recentRows);
            ReplaceSidebarRows(_shell.WorkTaskEntries, taskRows);
            ReplaceSidebarRows(_shell.ProjectEntries, treeRows);
            ReplaceSidebarRows(_shell.ViewModel.RecentConversations, historyRows);
            SetShellField("_renderingProjects", originalRenderingProjects);
            // Native hover targets must fit the monitor; screenshots alone do not establish pointer reachability.
            // 原生悬停目标须位于显示器工作区内，截图本身不能证明指针可达。
            var workArea = Microsoft.UI.Windowing.DisplayArea.GetFromWindowId(Window.AppWindow.Id,
                Microsoft.UI.Windowing.DisplayAreaFallback.Primary).WorkArea;
            double scale = _shell.XamlRoot.RasterizationScale;
            Window.AppWindow.MoveAndResize(new Windows.Graphics.RectInt32(workArea.X + 16, workArea.Y + 16,
                Math.Min((int)Math.Ceiling(1440 * scale), workArea.Width - 32),
                Math.Min((int)Math.Ceiling(900 * scale), workArea.Height - 32)));
            layout.SidebarCollapsed = false;
            layout.WorkRecentExpanded = true;
            layout.WorkNavigationRatio = 0.65;
            layout.WorkRecentRatio = 0.45;
            tree.Visibility = Visibility.Visible;
            foreach (double width in new[] { 318d, ShellLayoutMetrics.SidebarMin })
            {
                layout.SidebarWidth = width;
                Call("ApplyLayout");
                workContent.Visibility = Visibility.Visible;
                chatContent.Visibility = Visibility.Collapsed;
                Call("UpdateWorkRecentVisibility");
                await WaitAsync(() => Math.Abs(Element<Grid>("SidebarPane").ActualWidth - width) < 1 &&
                    workContent.ActualHeight > 0 && NativeUi.IsVisible(Element<ListView>("WorkRecentHistory")),
                    "roundness fixture reaches the actual measured work sidebar width " + width);
                await SettleAsync();
                await CheckRoundedSidebarListAsync(Element<ListView>("WorkRecentHistory"), recentRows[0], width, observations);
                await CheckRoundedSidebarListAsync(Element<ListView>("WorkTaskHistory"), taskRows[0], width, observations);
                await CheckRoundedProjectTreeAsync(tree, parent, child, width, observations);
                await CapturePresentedAsync($"shell-sidebar-roundness-work-{width}.png");
                workContent.Visibility = Visibility.Collapsed;
                chatContent.Visibility = Visibility.Visible;
                await WaitAsync(() => NativeUi.IsVisible(history), "the real chat history template has native layout at width " + width);
                await SettleAsync();
                await CheckRoundedSidebarListAsync(history, historyRows[0], width, observations);
                Check(Prompt.Text == draft && ActiveId == originalChat && _shell.ActiveMessages.SequenceEqual(originalRows) &&
                    FormalState() == formalBefore && _gateway.Writes == 0,
                    "roundness state and scrolling checks preserve draft, message objects and complete formal JSON at width " + width);
            }
            File.WriteAllText(Path.Combine(_directory, "shell-sidebar-roundness.json"),
                JsonSerializer.Serialize(observations, new JsonSerializerOptions { WriteIndented = true }));
        }
        finally
        {
            SetShellField("_renderingProjects", true);
            ReplaceSidebarRows(_shell.WorkRecentEntries, originalRecent);
            ReplaceSidebarRows(_shell.WorkTaskEntries, originalTasks);
            ReplaceSidebarRows(_shell.ProjectEntries, originalProjects);
            ReplaceSidebarRows(_shell.ViewModel.RecentConversations, originalHistory);
            history.SelectedItem = originalSelection;
            SetShellField("_renderingProjects", originalRenderingProjects);
            layout.SidebarWidth = originalWidth;
            layout.SidebarCollapsed = originalCollapsed;
            layout.WorkRecentExpanded = originalRecentExpanded;
            layout.WorkNavigationRatio = originalNavigationRatio;
            layout.WorkRecentRatio = originalRecentRatio;
            workContent.Visibility = originalWorkVisibility;
            chatContent.Visibility = originalChatVisibility;
            tree.Visibility = originalTreeVisibility;
            Window.AppWindow.MoveAndResize(new Windows.Graphics.RectInt32(originalWindowPosition.X, originalWindowPosition.Y,
                originalWindowSize.Width, originalWindowSize.Height));
            Call("ApplyLayout");
            Call("UpdateWorkRecentVisibility");
            NativeUi.SetText(Prompt, originalDraft);
            await SettleAsync();
            if (GetForegroundWindow() == windowHandle) SetCursorPos(originalPointer.X, originalPointer.Y);
        }
    }

    private async Task CheckRoundedSidebarListAsync(ListView list, object firstItem, double width, Dictionary<string, object> observations)
    {
        list.ScrollIntoView(firstItem);
        list.UpdateLayout();
        await WaitAsync(() => list.ContainerFromItem(firstItem) is ListViewItem container && container.ActualWidth > 0,
            list.Name + " realizes its actual first row at width " + width);
        await SettleAsync();
        var item = (ListViewItem)list.ContainerFromItem(firstItem);
        var presenter = NativeUi.Descendants<ListViewItemPresenter>(item).Single();
        var row = NativeUi.Descendants<Grid>(item).Single(grid => ReferenceEquals(grid.Tag, firstItem));
        var title = NativeUi.ByName<TextBlock>(row, "ProjectRowTitle");
        var actions = NativeUi.ByName<StackPanel>(row, "ProjectRowActions");
        var action = actions.Children.OfType<Button>().First();
        var scroll = NativeUi.Descendants<ScrollViewer>(list).First();
        string key = list.Name + "-" + width;
        Check(scroll.ScrollableHeight > 40 && scroll.ViewportHeight > 0,
            key + " uses enough view-only rows to exercise its real scroll viewport");
        scroll.ChangeView(null, scroll.ScrollableHeight, null, disableAnimation: true);
        await WaitAsync(() => scroll.VerticalOffset > 20, key + " really scrolls through native rows");
        list.ScrollIntoView(firstItem);
        await WaitAsync(() => list.ContainerFromItem(firstItem) is ListViewItem current && current.ActualWidth > 0 && scroll.VerticalOffset < 5,
            key + " restores the measured first row after native scrolling");
        item = (ListViewItem)list.ContainerFromItem(firstItem);
        presenter = NativeUi.Descendants<ListViewItemPresenter>(item).Single();
        row = NativeUi.Descendants<Grid>(item).Single(grid => ReferenceEquals(grid.Tag, firstItem));
        title = NativeUi.ByName<TextBlock>(row, "ProjectRowTitle");
        actions = NativeUi.ByName<StackPanel>(row, "ProjectRowActions");
        action = actions.Children.OfType<Button>().First();
        // WinUI inserts a separate backplate with its own inset; the presenter's bounds include that whitespace.
        // WinUI 插入独立且有内缩的背景层，presenter 的整体边界包含留白，不能拿它冒充实际背景。
        var backplate = NativeUi.Descendants<Border>(presenter)
            .Single(border => ReferenceEquals(VisualTreeHelper.GetParent(border), presenter));
        Check(IsEightDips(item.CornerRadius) && IsEightDips(presenter.CornerRadius) && IsEightDips(backplate.CornerRadius),
            key + " gives the actual native container and presenter the same eight-DIP corners");
        Rect surface = SidebarBounds(backplate, list);
        Check(surface.Left >= -0.5 && surface.Right <= list.ActualWidth - 15 && item.Margin.Right >= 16 &&
            SidebarBounds(presenter, item).Left >= -0.5 && SidebarBounds(presenter, item).Right <= item.ActualWidth + 0.5,
            key + " keeps the entire rounded surface inside the list with a separate scrollbar gutter");
        var selection = (SolidColorBrush)Application.Current.Resources["KynxaSelectionBrush"];
        Brush[] states = [presenter.PointerOverBackground, presenter.PressedBackground, presenter.SelectedBackground,
            presenter.SelectedPointerOverBackground, presenter.SelectedPressedBackground];
        Check((item.Background is null || item.Background is SolidColorBrush normal && normal.Color.A == 0) &&
            states.All(brush => brush is SolidColorBrush solid && solid.Color == selection.Color && solid.Color.A == 255),
            key + " keeps normal transparent and resolves hover, pressed and selected variants to the same opaque native background");
        action.Focus(FocusState.Keyboard);
        await WaitAsync(() => actions.Opacity > 0.99 && actions.IsHitTestVisible, key + " reveals the real action through keyboard focus");
        Check(title.Text.Length > 80 && title.TextTrimming == TextTrimming.CharacterEllipsis && title.IsTextTrimmed &&
            SidebarBounds(title, row).Right <= SidebarBounds(actions, row).Left + 1 &&
            SidebarBounds(action, row).Right <= row.ActualWidth + 0.5 && SidebarBounds(action, row).Left >= 0,
            key + " truncates only the long visible title and keeps the actual action button within a separate region");

        Prompt.Focus(FocusState.Programmatic);
        MovePointerInside(Element<Grid>("ShellGrid"), 0.75, 0.025);
        await WaitAsync(() => backplate.Background is null || backplate.Background is SolidColorBrush normalBrush && normalBrush.Color.A == 0,
            key + " reaches the actual transparent native normal background before hovering");
        MovePointerInside(item);
        await WaitAsync(() => backplate.Background is SolidColorBrush hoverBrush && hoverBrush.Color == selection.Color &&
            actions.Opacity > 0.99 && actions.IsHitTestVisible,
            key + " receives real mouse hover and paints its actual native backplate");
        Check(SidebarBounds(backplate, list) == surface && IsEightDips(backplate.CornerRadius),
            key + " paints real mouse hover within the measured rounded backplate");
        await CapturePresentedAsync($"shell-sidebar-roundness-{list.Name}-{width}-hover.png");
        MovePointerInside(Element<Grid>("ShellGrid"), 0.75, 0.025);
        await WaitAsync(() => backplate.Background is null || backplate.Background is SolidColorBrush normalBrush && normalBrush.Color.A == 0,
            key + " leaves the native hover before comparing the active or selected background");
        Border? active = null;
        if (firstItem is ProjectTreeEntry work)
        {
            work.IsActive = true;
            active = NativeUi.ByName<Border>(row, "WorkChatActiveBackground");
            await WaitAsync(() => active.Visibility == Visibility.Visible && active.ActualWidth > 0 && active.ActualHeight > 0 &&
                Math.Abs(active.ActualWidth - backplate.ActualWidth) < 1 && Math.Abs(active.ActualHeight - backplate.ActualHeight) < 1,
                key + " lays out a fixture active row without selecting a formal conversation");
            Rect activeBox = SidebarBounds(active, item), nativeBox = SidebarBounds(backplate, item);
            Check(IsEightDips(active.CornerRadius) && active.Margin.Left >= 0 && active.Margin.Top >= 0 &&
                active.Margin.Right >= 0 && active.Margin.Bottom >= 0 && SidebarBoundsMatch(activeBox, nativeBox),
                key + " aligns the active eight-DIP background with the native hover surface without negative margins: " +
                JsonSerializer.Serialize(new { activeBox, nativeBox, active.Margin, active.CornerRadius }));
        }
        else
        {
            list.SelectedItem = firstItem;
            await WaitAsync(() => item.IsSelected && backplate.Background is SolidColorBrush selectedBrush && selectedBrush.Color == selection.Color,
                key + " enters the native selected state without invoking a chat row");
            Check(IsEightDips(backplate.CornerRadius) && SidebarBounds(backplate, list) == surface,
                key + " keeps selection within the exact same rounded surface and scrollbar gutter");
        }
        await CapturePresentedAsync($"shell-sidebar-roundness-{list.Name}-{width}-active-selected.png");
        observations[key] = new
        {
            surface, item.CornerRadius, presenterRadius = presenter.CornerRadius, nativeInset = backplate.Margin, item.Margin, item.Padding,
            active = active is null ? (Rect?)null : SidebarBounds(active, list), title = SidebarBounds(title, list),
            action = SidebarBounds(action, list), title.IsTextTrimmed, realMouseHover = true,
            scroll = new { scroll.ScrollableHeight, scroll.ViewportHeight, scroll.VerticalOffset }
        };
        if (firstItem is ProjectTreeEntry reset) reset.IsActive = false;
        else list.SelectedItem = null;
    }

    private async Task CheckRoundedProjectTreeAsync(TreeView tree, ProjectTreeEntry parent, ProjectTreeEntry child,
        double width, Dictionary<string, object> observations)
    {
        tree.UpdateLayout();
        await WaitAsync(() => tree.ContainerFromItem(parent) is TreeViewItem && tree.ContainerFromItem(child) is TreeViewItem,
            "the real project tree realizes its synthetic expanded parent and child at width " + width);
        foreach (var entry in new[] { parent, child })
        {
            var item = (TreeViewItem)tree.ContainerFromItem(entry);
            var root = NativeUi.ByName<Grid>(item, "ContentPresenterGrid");
            var pointer = NativeUi.ByName<Border>(root, "PointerRowBackground");
            var active = NativeUi.ByName<Border>(root, "ActiveRowBackground");
            Check(IsEightDips(item.CornerRadius) && IsEightDips(root.CornerRadius) && IsEightDips(pointer.CornerRadius) &&
                item.Margin.Right >= 16 && SidebarBounds(root, tree).Right <= tree.ActualWidth - 15,
                "project tree " + (ReferenceEquals(entry, parent) ? "parent" : "child") + " retains rounded inset geometry at width " + width);
            Prompt.Focus(FocusState.Programmatic);
            MovePointerInside(Element<Grid>("ShellGrid"), 0.75, 0.025);
            MovePointerInside(root);
            var common = VisualStateManager.GetVisualStateGroups(root).Single(group => group.Name == "CommonStates");
            await WaitAsync(() => common.CurrentState?.Name == "PointerOver" && pointer.Visibility == Visibility.Visible,
                "project tree receives native mouse hover at width " + width);
            Check(pointer.Margin.Left >= 0 && pointer.Margin.Top >= 0 &&
                SidebarBounds(pointer, tree).Right <= tree.ActualWidth - 15,
                "project tree exposes and applies its actual rounded hover state at width " + width);
            entry.IsActive = true;
            await WaitAsync(() => active.Visibility == Visibility.Visible && active.ActualWidth > 0 && active.ActualHeight > 0 &&
                Math.Abs(active.ActualWidth - pointer.ActualWidth) < 1 && Math.Abs(active.ActualHeight - pointer.ActualHeight) < 1,
                "project tree lays out its actual active background at width " + width);
            Check(IsEightDips(active.CornerRadius) && SidebarBoundsMatch(SidebarBounds(active, tree), SidebarBounds(pointer, tree)),
                "project tree keeps active and hovered backgrounds on the same rounded surface at width " + width + ": " +
                JsonSerializer.Serialize(new { active = SidebarBounds(active, tree), pointer = SidebarBounds(pointer, tree), active.Margin }));
            await CapturePresentedAsync($"shell-sidebar-roundness-tree-{(ReferenceEquals(entry, parent) ? "parent" : "child")}-{width}.png");
            observations["tree-" + (ReferenceEquals(entry, parent) ? "parent" : "child") + "-" + width] = new
            { bounds = SidebarBounds(pointer, tree), active = SidebarBounds(active, tree), item.CornerRadius,
                pointerRadius = pointer.CornerRadius, currentState = common.CurrentState?.Name, realMouseHover = true };
            entry.IsActive = false;
            MovePointerInside(Element<Grid>("ShellGrid"), 0.75, 0.025);
        }
    }

    private static Rect SidebarBounds(FrameworkElement element, FrameworkElement relativeTo) => element.TransformToVisual(relativeTo)
        .TransformBounds(new Rect(0, 0, element.ActualWidth, element.ActualHeight));

    private static bool IsEightDips(CornerRadius radius) => radius.TopLeft == 8 && radius.TopRight == 8 && radius.BottomLeft == 8 && radius.BottomRight == 8;

    private static bool SidebarBoundsMatch(Rect first, Rect second) => Math.Abs(first.Left - second.Left) < 1 &&
        Math.Abs(first.Right - second.Right) < 1 && Math.Abs(first.Top - second.Top) < 1 && Math.Abs(first.Bottom - second.Bottom) < 1;

    [DllImport("user32.dll")] private static extern bool SetForegroundWindow(nint windowHandle);

    private static void ReplaceSidebarRows<T>(ObservableCollection<T> target, IEnumerable<T> rows)
    {
        target.Clear();
        foreach (T row in rows) target.Add(row);
    }
}
