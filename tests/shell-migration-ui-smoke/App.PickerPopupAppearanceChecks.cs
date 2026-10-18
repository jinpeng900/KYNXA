using System.Reflection;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;
using Windows.UI;
using Windows.UI.ViewManagement;
using Rectangle = Microsoft.UI.Xaml.Shapes.Rectangle;

namespace KYNXA_Desktop;

public partial class App
{
    private async Task CheckDropdownSpacingAsync()
    {
        Window? settings = null;
        try
        {
            Call("StorageSettings_Click", _shell, new RoutedEventArgs());
            settings = ShellField<Window>("_storageSettingsWindow");
            var root = (FrameworkElement)settings.Content;
            await WaitAsync(() => root.IsLoaded && root.XamlRoot is not null,
                "focused dropdown checks load the test-owned production settings window");
            await CheckPopupAppearanceAsync(settings, root, dropdownsOnly: true);
            await CheckLocalModelsDropdownsAsync(settings, root);
        }
        finally
        {
            settings?.Close();
            await SettleAsync();
        }
    }

    private async Task CheckLocalModelsDropdownsAsync(Window settings, FrameworkElement settingsRoot)
    {
        string originalId = AppearanceService.Current.Id, originalMode = AppearanceService.Mode;
        double originalSize = AppearanceService.ChatFontSize;
        string? originalAccent = AppearanceService.CustomAccent;
        string draft = Prompt.Text;
        Guid? activeId = ActiveId;
        int writes = _gateway.Writes;
        bool originalReads = _gateway.EnableLocalModelReads;
        var home = ShellField<ScrollViewer>("_settingsHome");
        var host = ShellField<ContentControl>("_settingsPageHost");
        var homeMode = NativeUi.ById<ComboBox>(settingsRoot, "AppearanceModePicker");
        var homeFont = NativeUi.ById<ComboBox>(settingsRoot, "ChatFontSizePicker");
        var windowSize = settings.AppWindow.Size;
        string layout = System.Text.Json.JsonSerializer.Serialize(ShellField<LayoutState>("_layout"));
        var observations = new List<object>();
        _gateway.EnableLocalModelReads = true;
        try
        {
            foreach (string mode in new[] { "light", "dark" })
            {
                AppearanceService.ApplyPreferences(originalId, mode, originalSize, mode == "light" ? "#C02070" : "#16836B");
                await ActivatePopupSettingsAsync(settings);
                var entry = NativeUi.ById<Button>(settingsRoot, "LocalModelsSettingsButton");
                entry.StartBringIntoView(new BringIntoViewOptions { AnimationDesired = false });
                await SettleAsync();
                double homeOffset = home.VerticalOffset;
                int readsBefore = _gateway.LocalModelReads;
                NativeUi.Invoke(entry);
                await WaitAsync(() => _gateway.LocalModelReads > readsBefore && NativeUi.Descendants<ComboBox>(settingsRoot)
                    .Any(picker => Microsoft.UI.Xaml.Automation.AutomationProperties.GetAutomationId(picker) == "LocalModelsLoadPolicy"
                        && picker.IsEnabled && picker.SelectedItem is ComboBoxItem { Tag: "auto" }),
                    mode + " actual settings local-model route loads its opt-in read-only snapshot");
                Check(!NativeUi.ById<InfoBar>(settingsRoot, "LocalModelsNotice").IsOpen &&
                    NativeUi.ById<TextBlock>(settingsRoot, "LocalModelsInstallDirectory").Text == Path.Combine(_directory, "LocalModels"),
                    mode + " local-model page presents only the isolated empty model catalog without an error");
                foreach (string id in new[] { "LocalModelsLoadPolicy", "LocalModelsFilter" })
                {
                    var picker = NativeUi.ById<ComboBox>(settingsRoot, id);
                    object selection = picker.SelectedItem;
                    picker.IsDropDownOpen = true;
                    var selected = (ComboBoxItem)selection;
                    await WaitAsync(() => selected.IsLoaded && VisualTreeHelper.GetOpenPopupsForXamlRoot(settingsRoot.XamlRoot)
                        .Any(popup => popup.IsOpen && popup.Child is not null && NativeUi.Descendants<ComboBoxItem>(popup.Child)
                            .Any(item => ReferenceEquals(item, selected))), mode + " " + id + " opens its production dropdown");
                    await SettleAsync();
                    var popup = NativePickerPopup(selected, settingsRoot.XamlRoot);
                    var border = NativeUi.ByName<Border>((FrameworkElement)popup.Child!, "PopupBorder");
                    var background = NativeUi.ByName<Grid>(selected, "LayoutRoot");
                    var scroll = NativeUi.Descendants<ScrollViewer>(border).Single();
                    Rect bounds = SidebarBounds(background, border);
                    double rightInset = border.ActualWidth - bounds.Right;
                    Check(border.CornerRadius.Equals(new CornerRadius(16)) && bounds.Left > 0 && bounds.Left <= 8 &&
                        rightInset > 0 && rightInset <= 8 && Math.Abs(bounds.Left - rightInset) < 1 &&
                        scroll.ScrollableHeight <= 1 && scroll.Padding.Right == 0,
                        mode + " " + id + " keeps rounded popup corners and compact symmetric sides without an idle scrollbar gutter");
                    foreach (string state in new[] { "Selected", "SelectedUnfocused", "SelectedPointerOver", "SelectedPressed" })
                    {
                        Check(VisualStateManager.GoToState(selected, state, false), mode + " " + id + " exposes " + state);
                        await SettleAsync();
                        CheckSelectedPopupItem(selected, mode + " " + id + " VSM " + state, observations);
                    }
                    // Exercise hover without selecting another policy, which would submit a production settings action.
                    // 只验证悬停，不切换策略选项，避免触发生产设置提交。
                    MovePointerInsideSettings(settings, settingsRoot, localPoint: new Point(12, 12));
                    await SettleAsync();
                    var common = VisualStateManager.GetVisualStateGroups(background).Single(group => group.Name == "CommonStates");
                    bool entered = false;
                    PointerEventHandler onEntered = (_, _) => entered = true;
                    selected.PointerEntered += onEntered;
                    try
                    {
                        MovePointerInsideSettings(settings, selected, popup);
                        await WaitAsync(() => entered && common.CurrentState?.Name == "SelectedPointerOver", mode + " " + id + " receives real selected-item hover");
                        CheckSelectedPopupItem(selected, mode + " " + id + " real hover", observations);
                        await RenderPopupElementAsync(border, "shell-popup-local-models-" + id + "-" + mode + "-ui-render.png");
                    }
                    finally { selected.PointerEntered -= onEntered; picker.IsDropDownOpen = false; }
                    await SettleAsync();
                    Check(ReferenceEquals(picker.SelectedItem, selection) && _gateway.Writes == writes,
                        mode + " " + id + " preserves the actual selection without submitting a policy change");
                }
                NativeUi.Invoke(NativeUi.ById<Button>(settingsRoot, "LocalModelsBack"));
                await WaitAsync(() => ReferenceEquals(host.Content, home) && home.IsLoaded, mode + " local-model Back restores the original settings homepage");
                await SettleAsync();
                Check(ReferenceEquals(homeMode, NativeUi.ById<ComboBox>(settingsRoot, "AppearanceModePicker")) &&
                    ReferenceEquals(homeFont, NativeUi.ById<ComboBox>(settingsRoot, "ChatFontSizePicker")) &&
                    Math.Abs(home.VerticalOffset - homeOffset) < 1 && settings.AppWindow.Size.Equals(windowSize),
                    mode + " local-model Back retains settings control instances, scroll position and window size");
            }
            Check(Prompt.Text == draft && ActiveId == activeId && _gateway.Writes == writes &&
                System.Text.Json.JsonSerializer.Serialize(ShellField<LayoutState>("_layout")) == layout,
                "local-model dropdown route preserves shell layout, draft and active conversation without gateway writes");
            File.WriteAllText(Path.Combine(_directory, "shell-popup-local-models.json"),
                System.Text.Json.JsonSerializer.Serialize(observations, new System.Text.Json.JsonSerializerOptions { WriteIndented = true }));
        }
        finally
        {
            _gateway.EnableLocalModelReads = originalReads;
            AppearanceService.ApplyPreferences(originalId, originalMode, originalSize, originalAccent);
        }
    }

    private async Task CheckScrollableFontGutterAsync(Window settings, FrameworkElement settingsRoot, string mode, List<object> observations)
    {
        var fontPicker = NativeUi.ById<ComboBox>(settingsRoot, "ChatFontSizePicker");
        await ActivatePopupSettingsAsync(settings);
        fontPicker.StartBringIntoView(new BringIntoViewOptions { AnimationDesired = false, VerticalAlignmentRatio = .2 });
        await SettleAsync();
        fontPicker.IsDropDownOpen = true;
        var selected = (ComboBoxItem)fontPicker.SelectedItem;
        await WaitAsync(() => selected.IsLoaded && VisualTreeHelper.GetOpenPopupsForXamlRoot(settingsRoot.XamlRoot)
            .Any(candidate => candidate.IsOpen && candidate.Child is not null &&
                NativeUi.Descendants<ComboBoxItem>(candidate.Child).Any(item => ReferenceEquals(item, selected)) &&
                NativeUi.Descendants<ScrollViewer>(candidate.Child).Any(scroll => scroll.ScrollableHeight > 0)),
            mode + " font dropdown realizes its production scrollable viewport");
        await SettleAsync();
        var popup = NativePickerPopup(selected, settingsRoot.XamlRoot);
        var popupBorder = NativeUi.ByName<Border>((FrameworkElement)popup.Child!, "PopupBorder");
        var selectedBackground = NativeUi.ByName<Grid>(selected, "LayoutRoot");
        var scrollbar = NativeUi.Descendants<ScrollBar>(popup.Child!).Single(bar =>
            bar.Orientation == Orientation.Vertical && bar.Visibility == Visibility.Visible && bar.Maximum > 0);
        var track = await ExpandPickerScrollbarAsync(scrollbar, settings, mode + " font dropdown");
        Rect backgroundBounds = SidebarBounds(selectedBackground, popupBorder), trackBounds = SidebarBounds(track, popupBorder);
        double gap = trackBounds.Left - backgroundBounds.Right;
        var content = NativeUi.Descendants<ItemsPresenter>(popupBorder).Single();
        File.WriteAllText(Path.Combine(_directory, "shell-popup-font-" + mode + "-geometry.json"),
            System.Text.Json.JsonSerializer.Serialize(new
            {
                backgroundBounds, trackBounds, GapDips = gap, TrackWidth = track.ActualWidth, track.Opacity,
                ContentMargin = content.Margin, ContentBounds = SidebarBounds(content, popupBorder),
                SelectedMargin = selected.Margin, SelectedBounds = SidebarBounds(selected, popupBorder),
                ResourceMargin = Application.Current.Resources["KynxaComboBoxItemMargin"]
            }, new System.Text.Json.JsonSerializerOptions { WriteIndented = true }));
        await RenderPopupElementAsync(popupBorder, "shell-popup-font-" + mode + "-ui-render.png");
        Check(track.ActualWidth > 0 && track.Opacity > .99 && gap >= 3.5 && gap <= 5.5 && backgroundBounds.Left <= 8,
            mode + " font selected background keeps compact sides and about four DIPs before the expanded native scrollbar track: " + gap);
        CheckSelectedPopupItem(selected, mode + " font actual scrollbar gutter", observations);
        observations.Add(new { Label = mode + " font scrollbar geometry", backgroundBounds, trackBounds, GapDips = gap });
        fontPicker.IsDropDownOpen = false;
        await SettleAsync();
    }

    private async Task CheckPickerPopupAppearanceAsync(Window settings, FrameworkElement settingsRoot, List<object> observations)
    {
        string paletteId = AppearanceService.Current.Id, originalMode = AppearanceService.Mode;
        string? originalAccent = AppearanceService.CustomAccent;
        double fontSize = AppearanceService.ChatFontSize;
        string draft = Prompt.Text;
        Guid? activeId = ActiveId;
        int writes = _gateway.Writes;
        var fontPicker = NativeUi.ById<ComboBox>(settingsRoot, "ChatFontSizePicker");
        try
        {
            foreach (string mode in new[] { "light", "dark" })
            {
                AppearanceService.ApplyPreferences(paletteId, mode, fontSize, mode == "light" ? "#C02070" : "#16836B");
                await CheckScrollableFontGutterAsync(settings, settingsRoot, mode, observations);

                await ActivateOwnedShortcutWindowAsync();
                foreach (string route in new[] { "ModelPickerButton", "ModelStatusButton" })
                {
                    var entry = Element<Button>(route);
                    Check(NativeUi.IsVisible(entry) && entry.ActualWidth > 0, mode + " " + route + " is a visible production entry");
                    NativeUi.Invoke(entry);
                    await WaitAsync(() => HasPopup<TextBox>("ModelPickerSearchBox"), mode + " " + route + " opens the production picker");
                    var search = Popup<TextBox>("ModelPickerSearchBox");
                    await CheckPickerSearchBoxAsync(search, mode + " " + route);
                    await CheckNativePickerListAsync(Popup<ListView>("ModelPickerList"), mode + " " + route, observations);
                    var configure = Popup<Button>("ConfigureCustomModelsButton");
                    Check(IsEightDips(configure.CornerRadius), mode + " " + route + " retains the rounded footer action");
                    await RenderPopupElementAsync((FrameworkElement)PickerFlyoutPresenter(search).Content,
                        "shell-popup-model-" + mode + "-" + route + "-ui-render.png");
                    ShellField<Flyout>("_modelPickerFlyout").Hide();
                    await WaitAsync(() => !HasPopup<TextBox>("ModelPickerSearchBox"), mode + " closes the model popup without choosing a model");
                }

                foreach (string route in new[] { "HistorySearchButton", "CtrlShiftK" })
                {
                    if (route == "HistorySearchButton")
                    {
                        var entry = Element<Button>(route);
                        Check(NativeUi.IsVisible(entry) && entry.ActualWidth > 0, mode + " " + route + " is a visible production entry");
                        NativeUi.Invoke(entry);
                    }
                    else
                    {
                        Prompt.Focus(FocusState.Programmatic);
                        DispatchShortcut(0x4B, shift: true);
                    }
                    await WaitAsync(() => HasPopup<TextBox>("HistorySearchBox"), mode + " " + route + " opens production history search");
                    var search = Popup<TextBox>("HistorySearchBox");
                    await CheckPickerSearchBoxAsync(search, mode + " " + route);
                    await CheckNativePickerListAsync(Popup<ListView>("HistorySearchResults"), mode + " " + route, observations);
                    await RenderPopupElementAsync((FrameworkElement)PickerFlyoutPresenter(search).Content,
                        "shell-popup-history-" + mode + "-" + route + "-ui-render.png");
                    ShellField<Flyout>("_historySearchFlyout").Hide();
                    await WaitAsync(() => !HasPopup<TextBox>("HistorySearchBox"), mode + " closes history search without invoking a result");
                }

                NativeUi.Invoke(Element<Button>("PermissionPickerButton"));
                await WaitAsync(() => HasPopup<Button>("PermissionOption_ask"), mode + " permission entry opens its production buttons");
                var permission = Popup<Button>("PermissionOption_ask");
                Check(PickerFlyoutPresenter(permission).CornerRadius.Equals(new CornerRadius(16)) &&
                    NativeUi.Descendants<Button>(PopupRoot(permission)).Where(button =>
                        Microsoft.UI.Xaml.Automation.AutomationProperties.GetAutomationId(button).StartsWith("PermissionOption_", StringComparison.Ordinal))
                        .All(button => IsEightDips(button.CornerRadius)), mode + " permission popup keeps rounded native action buttons");
                NativePickerPopup(permission, _shell.XamlRoot).IsOpen = false;
                await SettleAsync();
            }
            Check(Prompt.Text == draft && ActiveId == activeId && _gateway.Writes == writes,
                "picker appearance routes preserve the draft and selected conversation without formal gateway writes");
        }
        finally
        {
            fontPicker.IsDropDownOpen = false;
            ShellField<Flyout?>("_modelPickerFlyout")?.Hide();
            ShellField<Flyout?>("_historySearchFlyout")?.Hide();
            AppearanceService.ApplyPreferences(paletteId, originalMode, fontSize, originalAccent);
            settings.Activate();
            await SettleAsync();
        }
    }

    private static Popup NativePickerPopup(FrameworkElement element, XamlRoot root) =>
        VisualTreeHelper.GetOpenPopupsForXamlRoot(root).Single(popup => popup.IsOpen && popup.Child is not null &&
            NativeUi.Descendants<FrameworkElement>(popup.Child).Any(candidate => ReferenceEquals(candidate, element)));

    private FlyoutPresenter PickerFlyoutPresenter(FrameworkElement element) =>
        NativeUi.Descendants<FlyoutPresenter>(PopupRoot(element)).Single();

    private async Task CheckPickerSearchBoxAsync(TextBox search, string label)
    {
        await WaitAsync(() => search.IsLoaded && search.ActualWidth > 0, label + " realizes its native search textbox");
        search.ApplyTemplate();
        var border = NativeUi.ByName<Border>(search, "BorderElement");
        Check(IsEightDips(search.CornerRadius) && IsEightDips(border.CornerRadius) && border.ActualWidth > 0,
            label + " renders eight-DIP search corners in the actual SDK textbox template");
        Check(search.Focus(FocusState.Keyboard), label + " accepts native keyboard focus in its rounded search textbox");
        await WaitAsync(() => search.FocusState != FocusState.Unfocused, label + " retains native search focus");
    }

    private async Task<Rectangle> ExpandPickerScrollbarAsync(ScrollBar scrollbar, Window owner, string label)
    {
        // Hover the verified owned popup so ScrollViewer keeps its native scrollbar expanded during measurement.
        // 悬停已核验归属的弹窗，使 ScrollViewer 在测量期间保持原生滚动条展开。
        scrollbar.ApplyTemplate();
        await WaitAsync(() => NativeUi.Descendants<Rectangle>(scrollbar).Any(candidate => candidate.Name == "VerticalTrackRect"),
            label + " realizes the native scrollbar template");
        var root = (FrameworkElement)owner.Content;
        // Reopening at the same screen position needs a fresh pointer entry to expand the new scrollbar.
        // 同一屏幕位置重新打开菜单时，先移出再进入，使新滚动条收到真实进入事件。
        MovePointerInsideSettings(owner, root, localPoint: new Point(12, 12));
        await SettleAsync();
        MovePointerInsideSettings(owner, scrollbar, NativePickerPopup(scrollbar, root.XamlRoot));
        var track = NativeUi.ByName<Rectangle>(scrollbar, "VerticalTrackRect");
        await WaitAsync(() => track.ActualWidth > 0 && track.Opacity > .99, label + " paints the expanded native scrollbar track");
        return track;
    }

    private async Task CheckNativePickerListAsync(ListView list, string label, List<object> observations)
    {
        Check(list.Items.Count > 0, label + " contains production results");
        double originalMaxHeight = list.MaxHeight;
        object? originalSelection = list.SelectedItem;
        try
        {
            // Only constrain this disposable popup's viewport when the small mock catalog fits without scrolling.
            // 模拟目录较小时只限制当前临时弹窗视口，使真实生产行与原生滚动条同时出现。
            var scroll = NativeUi.Descendants<ScrollViewer>(list).First();
            // Subpixel overflow can leave a hidden scrollbar with a positive range.
            // 亚像素溢出可能使隐藏滚动条仍有微小正数范围。
            bool constrainedViewport = scroll.ScrollableHeight <= 1;
            if (constrainedViewport) list.MaxHeight = 80;
            object value = list.SelectedItem ?? list.Items[0];
            list.SelectedItem = value;
            list.ScrollIntoView(value);
            list.UpdateLayout();
            await WaitAsync(() => list.ContainerFromItem(value) is ListViewItem { ActualWidth: > 0 }, label + " realizes its native selected row");
            await SettleAsync();
            var item = (ListViewItem)list.ContainerFromItem(value);
            item.Focus(FocusState.Keyboard);
            var presenter = NativeUi.Descendants<ListViewItemPresenter>(item).Single();
            var backplate = NativeUi.Descendants<Border>(presenter).Single(border => ReferenceEquals(VisualTreeHelper.GetParent(border), presenter));
            Check(IsEightDips(item.CornerRadius) && IsEightDips(presenter.CornerRadius) && IsEightDips(backplate.CornerRadius) &&
                PickerFlyoutPresenter(list).CornerRadius.Equals(new CornerRadius(16)), label + " rounds the real container, SDK presenter, inserted backplate and outer popup");
            (Brush brush, string resource)[] states =
            [
                (presenter.PointerOverBackground, "ComboBoxItemBackgroundPointerOver"),
                (presenter.PressedBackground, "ComboBoxItemBackgroundPressed"),
                (presenter.SelectedBackground, "ComboBoxItemBackgroundSelected"),
                (presenter.SelectedPointerOverBackground, "ComboBoxItemBackgroundSelectedPointerOver"),
                (presenter.SelectedPressedBackground, "ComboBoxItemBackgroundSelectedPressed"),
                (presenter.PointerOverForeground, "ComboBoxItemForegroundPointerOver"),
                (presenter.SelectedForeground, "ComboBoxItemForegroundSelected")
            ];
            Check(states.All(state => ReferenceEquals(state.brush, Application.Current.Resources[state.resource])),
                label + " resolves hover, pressed and selected presenter brushes from the live application popup palette");
            await WaitAsync(() => backplate.Background is SolidColorBrush brush && brush.Color == AppearanceService.GetBrush("ComboBoxItemBackgroundSelected").Color,
                label + " paints the actual selected native backplate with the application accent tint");
            CheckPickerSelectedText(item, backplate, label, observations);
            await WaitAsync(() => scroll.ScrollableHeight > 1 && scroll.ComputedVerticalScrollBarVisibility == Visibility.Visible,
                label + " lays out a genuinely scrollable native viewport");
            var scrollbar = NativeUi.Descendants<ScrollBar>(scroll).Single(bar =>
                bar.Orientation == Orientation.Vertical && NativeUi.IsVisible(bar) && bar.ActualWidth > 0 &&
                bar.ActualHeight > 0 && bar.Maximum > 1);
            var track = await ExpandPickerScrollbarAsync(scrollbar, Window, label);
            Rect backgroundBounds = SidebarBounds(backplate, list), trackBounds = SidebarBounds(track, list);
            Check(item.Margin.Right >= 16 && trackBounds.Left - backgroundBounds.Right >= 2,
                label + " keeps a separate gutter between the real selected background and expanded scrollbar");
            observations.Add(new { Label = label, Surface = "ListViewItemPresenter", backgroundBounds, trackBounds,
                ConstrainedFixtureViewport = constrainedViewport, item.CornerRadius, PresenterRadius = presenter.CornerRadius,
                BackplateRadius = backplate.CornerRadius, Background = PopupBrushHex(backplate.Background),
                StateBrushes = states.Select(state => new { state.resource, Color = PopupBrushHex(state.brush) }).ToArray() });
            await CheckLivePickerHighContrastAsync(item, backplate, label, observations);
        }
        finally
        {
            list.SelectedItem = originalSelection;
            list.MaxHeight = originalMaxHeight;
            await SettleAsync();
        }
    }

    private void CheckPickerSelectedText(ListViewItem item, Border backplate, string label, List<object> observations)
    {
        var selectedForeground = AppearanceService.GetBrush("ComboBoxItemForegroundSelected");
        var texts = NativeUi.Descendants<TextBlock>(item).Where(text => text.Text.Length > 0 && NativeUi.IsVisible(text)).ToArray();
        Check(texts.Length >= 2, label + " realizes the selected primary and auxiliary production text");
        foreach (var text in texts)
        {
            double opacity = 1;
            for (DependencyObject? current = text; current is UIElement element && !ReferenceEquals(current, item); current = VisualTreeHelper.GetParent(current))
                opacity *= element.Opacity;
            var foreground = text.Foreground as SolidColorBrush;
            var effective = new SolidColorBrush(foreground is null ? Color.FromArgb(0, 0, 0, 0) :
                Color.FromArgb((byte)Math.Round(foreground.Color.A * opacity), foreground.Color.R, foreground.Color.G, foreground.Color.B));
            double contrast = PopupTextContrast(effective, backplate.Background, AppearanceService.Current.Sidebar);
            Check(foreground?.Color == selectedForeground.Color && contrast >= 4.5,
                label + " selected text inherits the native foreground and remains readable after opacity composition: " + contrast);
            observations.Add(new { Label = label, Surface = "SelectedText", text.Text, Foreground = PopupBrushHex(text.Foreground),
                EffectiveOpacity = opacity, Contrast = contrast });
        }
    }

    private async Task CheckLivePickerHighContrastAsync(ListViewItem item, Border backplate, string label, List<object> observations)
    {
        var colors = new Dictionary<UIElementType, Color>
        {
            [UIElementType.Window] = Color.FromArgb(255, 0, 0, 0), [UIElementType.WindowText] = Color.FromArgb(255, 255, 255, 255),
            [UIElementType.Highlight] = Color.FromArgb(255, 255, 255, 0), [UIElementType.HighlightText] = Color.FromArgb(255, 0, 0, 0),
            [UIElementType.Hotlight] = Color.FromArgb(255, 0, 255, 255), [UIElementType.ButtonFace] = Color.FromArgb(255, 0, 0, 0),
            [UIElementType.GrayText] = Color.FromArgb(255, 128, 128, 128)
        };
        var palette = AppearanceService.ResolveHighContrastPalette(AppearanceService.Current, type => colors[type]);
        var apply = typeof(AppearanceService).GetMethod("ApplyPopupPalette", BindingFlags.Static | BindingFlags.NonPublic)!;
        try
        {
            // Recolor the same live brushes; do not change Windows or the persisted application appearance.
            // 重着色同一组活动画刷，不修改 Windows 设置或应用持久化外观。
            apply.Invoke(null, [palette]);
            await SettleAsync();
            Check(backplate.Background is SolidColorBrush background && background.Color == colors[UIElementType.Highlight],
                label + " live selected backplate follows supplied high-contrast Highlight");
            CheckPickerSelectedText(item, backplate, label + " supplied high contrast", observations);
        }
        finally
        {
            apply.Invoke(null, [AppearanceService.Current]);
            await SettleAsync();
        }
    }
}
