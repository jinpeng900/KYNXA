using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Dispatching;
using System.Runtime.InteropServices;
using Windows.Foundation;
using MemoryUiSmoke;

namespace AgentUiSmoke;

public partial class App
{
    private PivotItem? _presentedManagementItem;
    private int _observedManagementIndex;

    private void AttachManagementPresentationChecks()
    {
        var pivot = Element<Pivot>("AgentTabs");
        _observedManagementIndex = pivot.SelectedIndex;
        pivot.SelectionChanged += (_, _) =>
        {
            if (_observedManagementIndex == pivot.SelectedIndex) return;
            _observedManagementIndex = pivot.SelectedIndex;
            if (!ReferenceEquals(_presentedManagementItem, pivot.SelectedItem)) _presentedManagementItem = null;
            File.AppendAllText(ResultPath, $"PIVOT selection={pivot.SelectedIndex}\n");
        };
        pivot.PivotItemLoading += (_, args) =>
        {
            if (ReferenceEquals(args.Item, pivot.SelectedItem)) _presentedManagementItem = null;
            File.AppendAllText(ResultPath, $"PIVOT loading={args.Item.Name}; selected={pivot.SelectedIndex}\n");
        };
        pivot.PivotItemLoaded += (_, args) =>
        {
            if (ReferenceEquals(args.Item, pivot.SelectedItem)) _presentedManagementItem = args.Item;
            File.AppendAllText(ResultPath, $"PIVOT loaded={args.Item.Name}; selected={pivot.SelectedIndex}\n");
        };
        pivot.PivotItemUnloading += (_, args) =>
        {
            if (ReferenceEquals(args.Item, _presentedManagementItem)) _presentedManagementItem = null;
        };
    }

    private bool IsManagementControlOnScreen(FrameworkElement control)
    {
        if (!control.IsLoaded || !NativeUi.IsVisible(control)) return false;
        for (DependencyObject? ancestor = control; ancestor is not null; ancestor = VisualTreeHelper.GetParent(ancestor))
            if (ancestor is UIElement element && (!element.IsHitTestVisible || element.Opacity < 0.99)) return false;
        var bounds = control.TransformToVisual(_root).TransformBounds(new Rect(0, 0, control.ActualWidth, control.ActualHeight));
        return bounds.Width > 40 && bounds.Height > 20 && bounds.Left >= -1 && bounds.Top >= 0 &&
            bounds.Right <= _root.ActualWidth + 1 && bounds.Bottom <= _root.ActualHeight + 1;
    }

    private async Task WaitForVisibleManagementSectionAsync(bool skills)
    {
        var pivot = Element<Pivot>("AgentTabs");
        await WaitAsync(() => pivot.SelectedIndex == (skills ? 1 : 0) &&
            ReferenceEquals(_presentedManagementItem, pivot.SelectedItem) && Button("AgentRefreshButton").IsEnabled &&
            IsManagementControlOnScreen(Element<ScrollViewer>(skills ? "AgentSkillEditor" : "AgentServerEditor")) &&
            IsManagementControlOnScreen(Element<TextBox>(skills ? "AgentSkillSearchBox" : "AgentServerSearchBox")),
            skills ? "selected skill PivotItem has fully loaded interactive controls inside the native viewport"
                : "selected MCP PivotItem has fully loaded interactive controls inside the native viewport");
        await WaitForRenderedManagementFrameAsync();
    }

    private async Task WaitForRenderedManagementFrameAsync()
    {
        // Pivot selection and child layout can precede presentation. Observe a rendered frame instead of a timer.
        // Pivot 选择及子项布局可能先于屏幕呈现；等待真正完成的渲染帧，不用固定延迟代替。
        var rendered = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        EventHandler<object> renderingHandler = (_, _) => { };
        EventHandler<RenderedEventArgs> renderedHandler = (_, _) => rendered.TrySetResult();
        CompositionTarget.Rendering += renderingHandler;
        CompositionTarget.Rendered += renderedHandler;
        try
        {
            if (!_root.DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () => _root.UpdateLayout()))
                throw new InvalidOperationException("The management layout dispatcher is no longer available.");
            await rendered.Task.WaitAsync(TimeSpan.FromSeconds(5));
            int result = FlushDesktopComposition();
            if (result < 0) throw new InvalidOperationException($"DwmFlush failed: 0x{result:X8}");
        }
        finally
        {
            CompositionTarget.Rendering -= renderingHandler;
            CompositionTarget.Rendered -= renderedHandler;
        }
        File.AppendAllText(ResultPath, $"PRESENTED item={_presentedManagementItem?.Name}; selected={Element<Pivot>("AgentTabs").SelectedIndex}\n");
    }

    [DllImport("dwmapi.dll", EntryPoint = "DwmFlush")]
    private static extern int FlushDesktopComposition();

    private async Task CheckManagementSearchAndLayoutAsync()
    {
        var window = _window ?? throw new InvalidOperationException("The isolated management window is unavailable.");
        var search = Element<TextBox>("AgentServerSearchBox");
        var list = Element<ListView>("AgentServerList");
        var command = Element<TextBox>("AgentServerCommandBox");
        string commandBefore = command.Text;
        string idBefore = Element<TextBox>("AgentServerIdBox").Text;
        int savesBefore = _api.Saves, connectionsBefore = _api.Connections;
        Check(!Button("AgentSaveServerButton").IsEnabled && Element<TextBlock>("AgentPendingLabel").Visibility == Visibility.Collapsed,
            "unchanged saved configuration has no pending label or redundant save action");
        NativeUi.SetText(command, "search-keeps-unsaved-command");
        NativeUi.SetText(search, "NO_MATCH_FIXTURE");
        await SettleAsync();
        Check(list.Items.Count == 0 && Element<TextBlock>("AgentServerEmptyLabel").Visibility == Visibility.Visible &&
            Element<TextBlock>("AgentServerFilteredHint").Visibility == Visibility.Visible &&
            command.Text == "search-keeps-unsaved-command" && Element<TextBox>("AgentServerIdBox").Text == idBefore && window.HasPendingChanges,
            "server search hides rows while preserving the stable identity and dirty editor");
        Check(Button("AgentClearServerFiltersButton").Visibility == Visibility.Visible && Button("AgentSaveServerButton").IsEnabled &&
            Element<TextBlock>("AgentPendingLabel").Text == "还有未保存的修改", "hidden server draft remains visibly unsaved and offers a filter reset");
        NativeUi.Invoke(Button("AgentClearServerFiltersButton")); await SettleAsync();
        Check(search.Text.Length == 0 && ((McpServerConfig)list.SelectedItem).Id == idBefore && command.Text == "search-keeps-unsaved-command" &&
            NativeUi.OpenDialog(_root) is null && _api.Saves == savesBefore, "native filter reset restores the dirty server without saving or a discard dialog");
        NativeUi.SetText(search, "NO_MATCH_FIXTURE"); await SettleAsync();
        UiText.Initialize("en"); await SettleAsync();
        Check(search.PlaceholderText == "Search server names or IDs" && search.Text == "NO_MATCH_FIXTURE" &&
            command.Text == "search-keeps-unsaved-command" && Element<TextBlock>("AgentPendingLabel").Text == "Unsaved changes",
            "server search, visible dirty status and hidden draft survive live language changes");
        UiText.Initialize("zh-CN");
        NativeUi.SetText(search, idBefore.ToUpperInvariant()); await SettleAsync();
        Check(list.Items.Count == 1 && ((McpServerConfig)list.SelectedItem).Id == idBefore && command.Text == "search-keeps-unsaved-command",
            "server ID matching ignores case and restores list selection without resetting edits");
        NativeUi.SetText(search, string.Empty);
        Element<ComboBox>("AgentServerSourceBox").SelectedIndex = 1; await SettleAsync();
        Check(list.Items.Count == 0 && command.Text == "search-keeps-unsaved-command" && window.HasPendingChanges && NativeUi.OpenDialog(_root) is null,
            "source filtering is presentation only and does not discard or confirm a hidden editor");
        Element<ComboBox>("AgentServerSourceBox").SelectedIndex = 0; await SettleAsync();
        NativeUi.Invoke(Button("AgentCancelServerButton")); await SettleAsync();
        Check(command.Text == commandBefore && !window.HasPendingChanges && _api.Saves == savesBefore && _api.Connections == connectionsBefore,
            "clearing filters and cancelling the draft perform no gateway writes or MCP startup");
        Check(!Button("AgentSaveServerButton").IsEnabled && Element<TextBlock>("AgentPendingLabel").Visibility == Visibility.Collapsed,
            "cancelling a saved draft clears its persistent pending feedback");
        await CheckManagementBusyFeedbackAsync();

        double scale = _root.XamlRoot?.RasterizationScale ?? 1;
        window.AppWindow.Resize(new Windows.Graphics.SizeInt32((int)Math.Ceiling(600 * scale), (int)Math.Ceiling(740 * scale)));
        await WaitAsync(() => Grid.GetColumn(Element<ScrollViewer>("AgentServerEditor")) == 0, "narrow management window stacks the server list above its editor");
        await WaitForVisibleManagementSectionAsync(skills: false);
        Check(Grid.GetRow(Element<ScrollViewer>("AgentServerEditor")) == 2 && Grid.GetRow(Button("AgentAddPresetButton")) == 1 &&
            Grid.GetRow(Button("AgentNewServerButton")) == 1 && Element<ComboBox>("AgentPresetBox").ActualWidth > 50 &&
            command.Text == commandBefore && ((McpServerConfig)list.SelectedItem).Id == idBefore,
            "narrow toolbar wraps and keeps the selected server and editor values");
        await NativeWindowCapture.CaptureAsync(window, Path.Combine(_directory, "management-narrow-mcp-zh.png"));
        window.ShowSection(skills: true);
        await WaitForVisibleManagementSectionAsync(skills: true);
        Check(Grid.GetColumn(Element<ScrollViewer>("AgentSkillEditor")) == 0 && Grid.GetRow(Element<ScrollViewer>("AgentSkillEditor")) == 3,
            "narrow skill management places its editor below the filtered list");
        await NativeWindowCapture.CaptureAsync(window, Path.Combine(_directory, "management-narrow-zh.png"));
        window.ShowSection(skills: false);
        window.AppWindow.Resize(new Windows.Graphics.SizeInt32((int)Math.Ceiling(940 * scale), (int)Math.Ceiling(720 * scale)));
        await WaitAsync(() => Grid.GetColumn(Element<ScrollViewer>("AgentServerEditor")) == 1, "widening management restores the two-column layout");
        await WaitForVisibleManagementSectionAsync(skills: false);
        Check(Grid.GetRow(Button("AgentAddPresetButton")) == 0 && command.Text == commandBefore && _api.Saves == savesBefore,
            "adaptive layout restores the toolbar without modifying configuration");
        await CheckShortManagementWindowAsync();
    }

    private async Task CheckSkillSearchAndSectionAsync()
    {
        var window = _window ?? throw new InvalidOperationException("The isolated management window is unavailable.");
        var list = Element<ListView>("AgentSkillList");
        var search = Element<TextBox>("AgentSkillSearchBox");
        var enabled = Element<CheckBox>("AgentSkillEnabledBox");
        var preview = Element<TextBox>("AgentSkillPreviewBox");
        string idBefore = ((AgentSkill)list.SelectedItem).Id;
        string previewBefore = preview.Text;
        int savesBefore = _api.Saves, readsBefore = _api.PreviewReads;
        enabled.IsChecked = false;
        NativeUi.SetText(search, "NO_SKILL_MATCH_FIXTURE"); await SettleAsync();
        Check(list.Items.Count == 0 && Element<TextBlock>("AgentSkillEmptyLabel").Visibility == Visibility.Visible &&
            Element<TextBlock>("AgentSkillFilteredHint").Visibility == Visibility.Visible &&
            enabled.IsChecked == false && preview.Text == previewBefore && window.HasPendingChanges,
            "skill search preserves the hidden skill toggle draft and its read-only preview");
        NativeUi.Invoke(Button("AgentClearSkillSearchButton")); await SettleAsync();
        Check(search.Text.Length == 0 && ((AgentSkill)list.SelectedItem).Id == idBefore && enabled.IsChecked == false &&
            preview.Text == previewBefore && _api.PreviewReads == readsBefore && _api.Saves == savesBefore,
            "native skill search reset restores the same dirty toggle without preview reload or saving");
        NativeUi.SetText(search, "NO_SKILL_MATCH_FIXTURE"); await SettleAsync();
        window.ShowSection(skills: false);
        await WaitForVisibleManagementSectionAsync(skills: false);
        window.ShowSection(skills: true);
        await WaitForVisibleManagementSectionAsync(skills: true);
        Check(Element<Pivot>("AgentTabs").SelectedIndex == 1 && enabled.IsChecked == false && search.Text == "NO_SKILL_MATCH_FIXTURE" &&
            preview.Text == previewBefore && _api.PreviewReads == readsBefore,
            "navigation selects the requested existing section while preserving draft and preview");
        NativeUi.SetText(search, idBefore.ToUpperInvariant()); await SettleAsync();
        Check(list.Items.Count == 1 && ((AgentSkill)list.SelectedItem).Id == idBefore && enabled.IsChecked == false,
            "skill ID filtering restores the same selection without resetting an unsaved toggle");
        NativeUi.SetText(search, string.Empty);
        NativeUi.Invoke(Button("AgentCancelSkillButton")); await SettleAsync();
        Check(list.Items.Count == 3 && enabled.IsChecked == true && !window.HasPendingChanges && _api.Saves == savesBefore,
            "skill search and section switching never save configuration implicitly");
    }

    private async Task CheckShortManagementWindowAsync()
    {
        var serverEditor = Element<ScrollViewer>("AgentServerEditor");
        var skillEditor = Element<ScrollViewer>("AgentSkillEditor");
        var directories = Element<ListView>("AgentDirectoryList");
        var directoryExpander = Element<Expander>("AgentSkillDirectoriesExpander");
        int savesBefore = _api.Saves, connectionsBefore = _api.Connections;
        object? directoryBefore = directories.SelectedItem;
        bool expandedBefore = directoryExpander.IsExpanded;
        double scale = _root.XamlRoot?.RasterizationScale ?? 1;
        try
        {
            UiText.Initialize("en");
            _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32((int)Math.Ceiling(480 * scale), (int)Math.Ceiling(540 * scale)));
            await WaitForVisibleManagementSectionAsync(skills: false);
            Check(serverEditor.ViewportHeight >= 60 && Element<Grid>("AgentServerListPanel").MaxHeight >= 92 &&
                Element<StackPanel>("AgentServerEditorBody").Children.Contains(Element<StackPanel>("AgentConnectionPanel")),
                "short narrow MCP window keeps selectable rows and scrolls connection controls with the form");
            Element<Expander>("AgentServerAdvanced").IsExpanded = true;
            await SettleAsync();
            serverEditor.ChangeView(null, serverEditor.ScrollableHeight, null, disableAnimation: true);
            await WaitForRenderedManagementFrameAsync();
            Check(serverEditor.ScrollableHeight > 0 && serverEditor.VerticalOffset > 0,
                "long MCP form can scroll to its actions in a short native window");
            var save = Button("AgentSaveServerButton");
            var saveOrigin = save.TransformToVisual(serverEditor).TransformPoint(new Point(0, 0));
            serverEditor.ChangeView(null, serverEditor.VerticalOffset + saveOrigin.Y, null, disableAnimation: true);
            await WaitForRenderedManagementFrameAsync();
            var saveBounds = save.TransformToVisual(serverEditor).TransformBounds(new Rect(0, 0, save.ActualWidth, save.ActualHeight));
            Check(saveBounds.Top >= -1 && saveBounds.Bottom <= serverEditor.ViewportHeight + 1,
                "server save action is fully reachable inside the short form viewport");
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "management-short-mcp-en.png"));
            serverEditor.ChangeView(null, 0, null, disableAnimation: true);
            Element<Expander>("AgentServerAdvanced").IsExpanded = false;
            _window.ShowSection(skills: true);
            await WaitForVisibleManagementSectionAsync(skills: true);
            directoryExpander.IsExpanded = true;
            await SettleAsync();
            Check(skillEditor.ViewportHeight >= 60 && Element<StackPanel>("AgentSkillEditorBody").Children.Contains(directoryExpander),
                "expanded directories share the skill editor scroll without consuming the whole short window");
            Check(!Button("AgentRemoveDirectoryButton").IsEnabled, "remove directory is unavailable until an actual path is selected");
            directories.SelectedItem = directories.Items[0]; await SettleAsync();
            directories.ScrollIntoView(directories.SelectedItem); await SettleAsync();
            var row = directories.ContainerFromItem(directories.SelectedItem) as ListViewItem;
            var label = row?.ContentTemplateRoot as TextBlock;
            Check(Button("AgentRemoveDirectoryButton").IsEnabled && label is not null && label.TextTrimming == TextTrimming.CharacterEllipsis &&
                label.TextWrapping == TextWrapping.NoWrap && ToolTipService.GetToolTip(label)?.ToString() == directories.SelectedItem.ToString(),
                "directory row fits a narrow column while its complete literal path remains in the tooltip");
            await WaitForRenderedManagementFrameAsync();
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "management-short-skills-en.png"));
            Check(_api.Saves == savesBefore && _api.Connections == connectionsBefore && !_window.HasPendingChanges,
                "short window scrolling, directory selection and language changes do not save or connect");
        }
        finally
        {
            directoryExpander.IsExpanded = expandedBefore;
            directories.SelectedItem = directoryBefore;
            UiText.Initialize("zh-CN");
            _window!.ShowSection(skills: false);
            _window.AppWindow.Resize(new Windows.Graphics.SizeInt32((int)Math.Ceiling(940 * scale), (int)Math.Ceiling(720 * scale)));
            await WaitForVisibleManagementSectionAsync(skills: false);
            Check(Element<Grid>("AgentServersGrid").Children.Contains(Element<StackPanel>("AgentConnectionPanel")) &&
                Element<Grid>("AgentSkillsGrid").Children.Contains(directoryExpander),
                "widening restores original control owners without rebuilding their state");
        }
    }

    private async Task CheckManagementBusyFeedbackAsync()
    {
        int readsBefore = _api.Reads, savesBefore = _api.Saves, connectionsBefore = _api.Connections;
        var delayed = new TaskCompletionSource<AgentConfig>(TaskCreationOptions.RunContinuationsAsynchronously);
        _api.DelayedConfig = delayed;
        try
        {
            NativeUi.Invoke(Button("AgentRefreshButton"));
            await WaitAsync(() => _api.Reads == readsBefore + 1 && !Button("AgentRefreshButton").IsEnabled,
                "real configuration refresh remains busy while its injected read is pending");
            Check(Element<ProgressRing>("AgentLoadingRing").IsActive && Element<TextBlock>("AgentPendingLabel").Text == "正在处理，请稍候。" &&
                !Element<TextBox>("AgentServerSearchBox").IsEnabled && !Button("AgentSaveServerButton").IsEnabled,
                "pending gateway read explains the busy state and disables conflicting edits");
            UiText.Initialize("en"); await SettleAsync();
            Check(Element<TextBlock>("AgentPendingLabel").Text == "Working. Please wait." && _api.Saves == savesBefore,
                "live translation keeps actual busy feedback without starting a save");
            delayed.TrySetResult(_api.Config);
            await WaitAsync(() => Button("AgentRefreshButton").IsEnabled && !_window!.HasPendingChanges,
                "completed configuration read restores the actual idle state");
            Check(!Element<ProgressRing>("AgentLoadingRing").IsActive && Element<TextBlock>("AgentPendingLabel").Visibility == Visibility.Collapsed &&
                _api.Saves == savesBefore && _api.Connections == connectionsBefore,
                "read completion clears busy feedback without writing or connecting");
        }
        finally
        {
            delayed.TrySetResult(_api.Config);
            _api.DelayedConfig = null;
            UiText.Initialize("zh-CN");
        }
    }
}
