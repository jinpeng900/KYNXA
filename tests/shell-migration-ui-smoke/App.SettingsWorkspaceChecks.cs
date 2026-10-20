using System.Reflection;
using System.Text.Json;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using MemoryUiSmoke;
using Microsoft.UI.Composition;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Windows.Foundation;

namespace KYNXA_Desktop;

public partial class App
{
    /// <summary>
    /// Uses actual settings controls with synthetic HTTP reads; no installer, supplier or personal data is involved.
    /// 使用实际设置控件和虚构 HTTP 读取，不运行安装、不调用供应商、不接触个人数据。
    /// </summary>
    private async Task CheckSettingsWorkspaceAsync()
    {
        Window? settings = null;
        string draft = Prompt.Text;
        Guid? chatId = ActiveId;
        string messages = JsonSerializer.Serialize(_shell.ActiveMessages.Select(row => row.Message));
        string originalMode = AppearanceService.Mode;
        var observations = new List<object>();
        _gateway.EnableSettingsReads = true;
        try
        {
            Call("StorageSettings_Click", _shell, new RoutedEventArgs());
            settings = ShellField<Window>("_storageSettingsWindow");
            var root = (FrameworkElement)settings.Content;
            await WaitAsync(() => root.IsLoaded && root.XamlRoot is not null,
                "the unified settings window loads its actual native content");
            var workspace = NativeUi.Descendants<SettingsWorkspace>(root).Single();
            var navigation = NativeUi.ById<StackPanel>(root, "SettingsNavigation");
            var host = NativeUi.ById<Grid>(root, "SettingsPageHost");
            string[] keys = ["general", "appearance", "memory", "local-models", "storage", "tools", "retrieval", "about"];
            Check(keys.All(key => NativeUi.ById<Button>(root, "SettingsCategory_" + key).CornerRadius.TopLeft >= 8),
                "all eight settings categories use the persistent rounded navigation");
            Check(workspace.SelectedKey == "general" && host.Children.Count == 1,
                "general settings load first while other categories stay lazy");
            var firstPage = (FrameworkElement)host.Children.Single();
            nint handle = WinRT.Interop.WindowNative.GetWindowHandle(settings);
            uint thread = PresentationWindowProcess(handle, out uint process);
            string nativeClass = PresentationClass(handle);
            HashSet<nint> VisibleWindows() => PresentationThreadWindows(thread, process, nativeClass)
                .Where(PresentationIsVisible).ToHashSet();
            var originalWindows = VisibleWindows();
            Check(originalWindows.Contains(handle) && originalWindows.Count == 2,
                "only the fixture main window and one settings window are visible before category navigation");

            async Task<FrameworkElement> SelectAsync(string key)
            {
                NativeUi.Invoke(NativeUi.ById<Button>(root, "SettingsCategory_" + key));
                await WaitAsync(() => workspace.SelectedKey == key && host.Children.OfType<FrameworkElement>().Any(page =>
                    AutomationProperties.GetAutomationId(page) == "SettingsPage_" + key && page.IsLoaded && NativeUi.IsVisible(page)),
                    "the actual settings navigation reveals " + key);
                await SettleAsync();
                Check(ReferenceEquals(navigation, NativeUi.ById<StackPanel>(root, "SettingsNavigation")) && NativeUi.IsVisible(navigation) &&
                    host.Children.OfType<FrameworkElement>().Count(page => page.Visibility == Visibility.Visible) == 1 &&
                    VisibleWindows().SetEquals(originalWindows),
                    key + " keeps the left navigation, one visible cached page and no extra management window");
                return host.Children.OfType<FrameworkElement>().Single(page => page.Visibility == Visibility.Visible);
            }

            Rect BoundsIn(FrameworkElement element, UIElement relativeTo) => element.TransformToVisual(relativeTo)
                .TransformBounds(new Rect(0, 0, element.ActualWidth, element.ActualHeight));
            void CheckNavigationLabels(string state)
            {
                var labels = keys.Select(key =>
                {
                    var button = NativeUi.ById<Button>(root, "SettingsCategory_" + key);
                    var label = ((Panel)button.Content).Children.OfType<TextBlock>().Single();
                    return new { Key = key, label.Text, Bounds = BoundsIn(label, button), label.DesiredSize,
                        ButtonWidth = button.ActualWidth, ButtonHeight = button.ActualHeight, button.Padding };
                }).ToArray();
                observations.Add(new { Page = "navigation", State = state, Language = UiText.Language,
                    RootWidth = root.ActualWidth, NavigationWidth = navigation.ActualWidth, Labels = labels });
                Check(labels.All(label => label.Bounds.Width > 0 && label.Bounds.Height > 0 &&
                    label.Bounds.Left >= label.Padding.Left - 1 && label.Bounds.Top >= label.Padding.Top - 1 &&
                    label.Bounds.Right <= label.ButtonWidth - label.Padding.Right + 1 &&
                    label.Bounds.Bottom <= label.ButtonHeight - label.Padding.Bottom + 1 &&
                    label.Bounds.Width >= label.DesiredSize.Width - 1 && label.Bounds.Height >= label.DesiredSize.Height - 1),
                    state + " navigation keeps all eight complete category labels inside their padded buttons without clipping");
            }
            var responsiveSizes = new Dictionary<(string State, string Page),
                (double PageWidth, double ContentWidth, double? HeaderActionRight)>();
            var responsivePalettes = new Dictionary<string, (double GridWidth, double CardWidth, int Columns)>();
            async Task CheckResponsivePagesAsync(string state)
            {
                foreach (string key in new[] { "general", "appearance", "storage", "about" })
                {
                    var page = await SelectAsync(key) as ScrollViewer
                        ?? throw new InvalidOperationException("A basic settings category has no actual scroll viewer.");
                    var frame = page.Content as Border
                        ?? throw new InvalidOperationException("The settings page has no common padded content frame.");
                    var content = frame.Child as FrameworkElement
                        ?? throw new InvalidOperationException("The settings page has no actual content.");
                    page.ChangeView(null, 0, null, true);
                    root.UpdateLayout();
                    await SettleAsync();
                    var heading = NativeUi.ById<TextBlock>(root, "SettingsPageHeading");
                    Rect headingBounds = BoundsIn(heading, host), contentBounds = BoundsIn(content, page);
                    Rect? headerActionBounds = key == "appearance"
                        ? BoundsIn(NativeUi.ById<Button>(root, "AppearanceResetButton"), host) : null;
                    string[] controlIds = key switch
                    {
                        "general" => ["InterfaceLanguagePicker"],
                        "appearance" => ["AppearanceModePicker", "ChatFontSizePicker", "AppearancePaletteGrid",
                            "AppearanceAccentInput", "AppearanceColorPickerButton", "AppearanceAccentApplyButton", "AppearanceAccentResetButton"],
                        "storage" => ["StorageDirectoryPath", "StorageDirectoryChooseButton", "ExtensionStorageDirectoryPath",
                            "ExtensionStorageDirectoryChooseButton"],
                        _ => []
                    };
                    var targets = controlIds.Select(id => NativeUi.ById<FrameworkElement>(page, id)).ToList();
                    if (content is Panel panel) targets.AddRange(panel.Children.OfType<FrameworkElement>());
                    if (key == "appearance") targets.AddRange(NativeUi.ById<Grid>(page, "AppearancePaletteGrid").Children.OfType<Button>());
                    var measured = targets.Where(element => element.Visibility == Visibility.Visible).Select(element => new
                    {
                        Id = AutomationProperties.GetAutomationId(element), Bounds = BoundsIn(element, page),
                        element.ActualWidth, element.DesiredSize.Width
                    }).ToArray();
                    observations.Add(new { State = state, Page = key, Scale = root.XamlRoot.RasterizationScale,
                        RootWidth = root.ActualWidth, HostWidth = host.ActualWidth, PageWidth = page.ActualWidth,
                        page.ViewportWidth, page.ExtentWidth, page.ScrollableWidth, Heading = headingBounds,
                        Content = contentBounds, Frame = BoundsIn(frame, page), ContentDesiredWidth = content.DesiredSize.Width,
                        HeaderAction = headerActionBounds, Controls = measured });
                    responsiveSizes[(state, key)] = (page.ActualWidth, contentBounds.Width, headerActionBounds?.Right);
                    Check(Math.Abs(headingBounds.Left - 24) <= 1 && Math.Abs(contentBounds.Left - 24) <= 1 &&
                        contentBounds.Width > 200 && Math.Abs(contentBounds.Right - (page.ActualWidth - 24)) <= 1 &&
                        Math.Abs(frame.ActualWidth - page.ActualWidth) <= 1 && page.ScrollableWidth <= 1 &&
                        measured.All(item => item.Bounds.Width > 0 && item.Bounds.Left >= 23 && item.Bounds.Right <= page.ActualWidth - 23),
                        state + " " + key + " fills the viewport between 24 DIP page insets and keeps cards and actions inside its bounds");
                    if (headerActionBounds is { } actionBounds)
                        Check(actionBounds.Width > 0 && Math.Abs(actionBounds.Right - (host.ActualWidth - 24)) <= 1 &&
                            headingBounds.Right <= actionBounds.Left - 11,
                            state + " appearance keeps the restore action at the right page inset without overlapping its heading");
                    if (key == "appearance")
                    {
                        var paletteGrid = NativeUi.ById<Grid>(page, "AppearancePaletteGrid");
                        var paletteCards = paletteGrid.Children.OfType<Button>().ToArray();
                        var cardBounds = paletteCards.Select(card => BoundsIn(card, paletteGrid)).ToArray();
                        var labels = paletteCards.SelectMany(card => NativeUi.Descendants<TextBlock>(card)
                            .Select(label => new { label.Text, label.IsTextTrimmed, Bounds = BoundsIn(label, card),
                                CardWidth = card.ActualWidth, CardHeight = card.ActualHeight,
                                label.ActualWidth, label.ActualHeight })).ToArray();
                        int columns = paletteGrid.ColumnDefinitions.Count;
                        observations.Add(new { Page = "appearance-palettes", State = state, Columns = columns,
                            Grid = BoundsIn(paletteGrid, page), CardBounds = cardBounds, Labels = labels });
                        responsivePalettes[state] = (paletteGrid.ActualWidth, paletteCards[0].ActualWidth, columns);
                        Check(paletteCards.Length == 5 && columns is >= 1 and <= 5 && columns != 4 &&
                            Math.Abs(paletteGrid.ActualWidth - content.ActualWidth) <= 1 &&
                            cardBounds.All(bounds => bounds.Width >= 170 && bounds.Height is >= 110 and <= 160 &&
                                bounds.Left >= -1 && bounds.Right <= paletteGrid.ActualWidth + 1 &&
                                bounds.Top >= -1 && bounds.Bottom <= paletteGrid.ActualHeight + 1) &&
                            paletteCards.All(card => card.CornerRadius.TopLeft >= 10),
                            state + " lays out all five rounded palette previews across the full page width with readable card sizes");
                        Check(cardBounds.All(bounds => Math.Abs(bounds.Width - cardBounds[0].Width) <= 1) &&
                            cardBounds.SelectMany((bounds, index) => cardBounds.Skip(index + 1).Select(other =>
                                bounds.Right <= other.Left + 1 || other.Right <= bounds.Left + 1 ||
                                bounds.Bottom <= other.Top + 1 || other.Bottom <= bounds.Top + 1)).All(separated => separated) &&
                            labels.All(label => !label.IsTextTrimmed && label.ActualWidth > 0 && label.ActualHeight > 0 &&
                                label.Bounds.Left >= -1 && label.Bounds.Right <= label.CardWidth + 1 &&
                                label.Bounds.Top >= -1 && label.Bounds.Bottom <= label.CardHeight + 1),
                            state + " keeps equal-width palette cards separate and their complete names and selection glyphs inside each card");
                        if (state is "wide" or "maximized")
                            Check(columns == 5 && cardBounds.All(bounds => Math.Abs(bounds.Top - cardBounds[0].Top) <= 1),
                                state + " places all five palette choices in a single balanced row");
                        if (state == "narrow")
                            Check(columns <= 3 && paletteGrid.RowDefinitions.Count >= 2,
                                "narrow settings wrap palette cards into readable rows instead of compressing five into one row");
                        if (state is "wide" or "narrow")
                        {
                            var themePicker = NativeUi.ById<SegmentedSelector>(page, "AppearanceModePicker");
                            string savedMode = AppearanceService.Mode;
                            try
                            {
                                foreach (string theme in new[] { "light", "dark" })
                                {
                                    themePicker.SelectedKey = theme;
                                    await SettleAsync();
                                    Check(AppearanceService.Mode == theme && paletteCards.Count(card =>
                                        AutomationProperties.GetItemStatus(card) == UiText.Get("已选中")) == 1,
                                        state + " " + theme + " previews follow the active theme and retain one selected palette");
                                    paletteGrid.StartBringIntoView(new BringIntoViewOptions { AnimationDesired = false,
                                        VerticalAlignmentRatio = 0.1 });
                                    await CaptureSettingsWorkspaceAsync(settings,
                                        "shell-settings-workspace-palettes-" + state + "-" + theme + ".png");
                                    await RenderPopupElementAsync(paletteGrid,
                                        "shell-palette-grid-" + state + "-" + theme + "-ui-render.png");
                                }
                            }
                            finally { themePicker.SelectedKey = savedMode; }
                        }
                    }
                    if (page.ScrollableHeight > 1)
                    {
                        page.ChangeView(null, page.ScrollableHeight, null, true);
                        await SettleAsync();
                        Rect bottom = BoundsIn(content, page);
                        Check(Math.Abs(page.VerticalOffset - page.ScrollableHeight) <= 1 && bottom.Bottom <= page.ActualHeight - 23 && bottom.Bottom > 0,
                            state + " " + key + " scrolls the full content bottom into view without clipping lower actions");
                    }
                }
                CheckNavigationLabels(state);
            }
            var presenter = (OverlappedPresenter)settings.AppWindow.Presenter;
            var initialBounds = new Windows.Graphics.RectInt32(settings.AppWindow.Position.X, settings.AppWindow.Position.Y,
                settings.AppWindow.Size.Width, settings.AppWindow.Size.Height);
            async Task ResizeSettingsAsync(double width, double height)
            {
                presenter.Restore();
                double scale = root.XamlRoot.RasterizationScale;
                var workArea = DisplayArea.GetFromWindowId(settings.AppWindow.Id, DisplayAreaFallback.Primary).WorkArea;
                int physicalWidth = Math.Min((int)Math.Ceiling(width * scale), workArea.Width - 32);
                int physicalHeight = Math.Min((int)Math.Ceiling(height * scale), workArea.Height - 32);
                settings.AppWindow.MoveAndResize(new Windows.Graphics.RectInt32(workArea.X + 16, workArea.Y + 16,
                    physicalWidth, physicalHeight));
                await SettleAsync();
            }
            // Observe the actual monitor scale; this regression never changes the user's Windows DPI settings.
            // 记录当前屏幕的真实缩放比例，本回归不修改用户 Windows DPI 设置。
            await CheckResponsivePagesAsync("initial");
            await ResizeSettingsAsync(1240, 760);
            await CheckResponsivePagesAsync("wide");
            await ResizeSettingsAsync(700, 460);
            await CheckResponsivePagesAsync("narrow");
            presenter.Maximize();
            await SettleAsync();
            await CheckResponsivePagesAsync("maximized");
            presenter.Restore();
            settings.AppWindow.MoveAndResize(initialBounds);
            await SettleAsync();
            await CheckResponsivePagesAsync("restored");
            // Compare the same cached pages across actual native window sizes, not only their final fit.
            // 对比同一缓存页面在真实窗口缩放前后的宽度，避免只校验最终边界而漏掉固定窄列。
            foreach (string key in new[] { "general", "appearance", "storage", "about" })
            {
                var initial = responsiveSizes[("initial", key)];
                var wide = responsiveSizes[("wide", key)];
                var narrow = responsiveSizes[("narrow", key)];
                var maximized = responsiveSizes[("maximized", key)];
                var restored = responsiveSizes[("restored", key)];
                Check(wide.PageWidth > narrow.PageWidth + 1 && wide.ContentWidth > narrow.ContentWidth + 1 &&
                    Math.Abs((wide.ContentWidth - narrow.ContentWidth) - (wide.PageWidth - narrow.PageWidth)) <= 2 &&
                    maximized.ContentWidth >= wide.ContentWidth - 1 &&
                    Math.Abs(restored.PageWidth - initial.PageWidth) <= 2 &&
                    Math.Abs(restored.ContentWidth - initial.ContentWidth) <= 2,
                    key + " expands with the viewport, shrinks without clipping and restores its original content width");
            }
            var wideAppearance = responsiveSizes[("wide", "appearance")];
            var narrowAppearance = responsiveSizes[("narrow", "appearance")];
            Check(wideAppearance.HeaderActionRight is { } wideActionRight &&
                narrowAppearance.HeaderActionRight is { } narrowActionRight &&
                Math.Abs((wideActionRight - narrowActionRight) - (wideAppearance.PageWidth - narrowAppearance.PageWidth)) <= 2,
                "the appearance restore action follows the page's right edge when the settings window changes width");
            var widePalette = responsivePalettes["wide"];
            var narrowPalette = responsivePalettes["narrow"];
            var maximizedPalette = responsivePalettes["maximized"];
            Check(widePalette.GridWidth > narrowPalette.GridWidth + 1 &&
                maximizedPalette.GridWidth >= widePalette.GridWidth - 1 &&
                maximizedPalette.CardWidth > widePalette.CardWidth + 1 &&
                Math.Abs(responsivePalettes["restored"].GridWidth - responsivePalettes["initial"].GridWidth) <= 2 &&
                responsivePalettes["restored"].Columns == responsivePalettes["initial"].Columns,
                "palette previews grow with the wider viewport, wrap at narrower widths and restore their original layout");

            var appearance = await SelectAsync("appearance");
            var mode = NativeUi.ById<SegmentedSelector>(appearance, "AppearanceModePicker");
            var palette = NativeUi.ById<Grid>(appearance, "AppearancePaletteGrid");
            var cards = palette.Children.OfType<Button>().ToArray();
            string originalPaletteId = AppearanceService.Current.Id;
            var selectedCard = cards.First(card => AutomationProperties.GetAutomationId(card) != "AppearancePalette_" + originalPaletteId);
            NativeUi.Invoke(selectedCard);
            Check(AppearanceService.Current.Id == AutomationProperties.GetAutomationId(selectedCard)["AppearancePalette_".Length..] &&
                new LayoutStateService().Load().AppearancePaletteId == AppearanceService.Current.Id &&
                AutomationProperties.GetItemStatus(selectedCard) == UiText.Get("已选中") &&
                cards.Count(card => AutomationProperties.GetItemStatus(card) == UiText.Get("已选中")) == 1,
                "invoking a responsive palette card saves its existing preference and exposes exactly one accessible selection");
            NativeUi.Invoke(cards.Single(card => AutomationProperties.GetAutomationId(card) == "AppearancePalette_" + originalPaletteId));
            Check(NativeUi.Descendants<RadioButton>(mode).Count() == 3 && mode.SelectedKey == originalMode,
                "theme mode uses three real radio segments with the saved selection");
            var capsule = typeof(SegmentedSelector).GetField("_capsule", PrivateInstance)?.GetValue(mode) as ShapeVisual
                ?? throw new InvalidOperationException("The loaded segment has no actual compositor capsule.");
            var beforeOffset = capsule.Offset;
            var labelPositions = NativeUi.Descendants<RadioButton>(mode).Select(button =>
                button.TransformToVisual(mode).TransformPoint(default)).ToArray();
            string nextMode = originalMode == "dark" ? "light" : "dark";
            var target = NativeUi.ById<RadioButton>(mode, "Segment_" + nextMode);
            await ActivateOwnedShortcutWindowAsync(settings);
            target.StartBringIntoView(new BringIntoViewOptions { AnimationDesired = false, VerticalAlignmentRatio = 0.15 });
            root.UpdateLayout();
            await SettleAsync();
            bool entered = false;
            Microsoft.UI.Xaml.Input.PointerEventHandler onEntered = (_, _) => entered = true;
            target.PointerEntered += onEntered;
            try
            {
                MovePointerInsideSettings(settings, root, localPoint: new Point(12, root.ActualHeight - 12));
                await SettleAsync();
                MovePointerInsideSettings(settings, target);
                await WaitAsync(() => entered, "the actual target segment receives native pointer input before clicking");
                // Recalculate after pointer dispatch and let WinUI process the press before releasing it.
                // 指针事件派发后重新计算坐标，让 WinUI 先处理按下再接收释放，避免缩放后点击时序丢失。
                MovePointerInsideSettings(settings, target);
                RequireSettingsForeground(settings);
                try
                {
                    SendFixtureInput([new NativeInput { Data = new NativeInputData { Mouse = new NativeMouseInput { Flags = 0x0002 } } }]);
                    await Task.Delay(40);
                }
                finally
                {
                    SendFixtureInput([new NativeInput { Data = new NativeInputData { Mouse = new NativeMouseInput { Flags = 0x0004 } } }]);
                }
            }
            finally { target.PointerEntered -= onEntered; }
            await WaitAsync(() => mode.SelectedKey == nextMode && AppearanceService.Mode == nextMode,
                "a real mouse click changes the selected mode and applies its actual appearance preference");
            await SettleAsync();
            var afterPositions = NativeUi.Descendants<RadioButton>(mode).Select(button =>
                button.TransformToVisual(mode).TransformPoint(default)).ToArray();
            var animation = capsule.ImplicitAnimations?["Offset"] as Vector3KeyFrameAnimation;
            Check(Math.Abs(capsule.Offset.X - beforeOffset.X) > 20 &&
                labelPositions.Zip(afterPositions).All(pair => Math.Abs(pair.First.X - pair.Second.X) < 1) &&
                (!new Windows.UI.ViewManagement.UISettings().AnimationsEnabled || animation?.Duration == TimeSpan.FromMilliseconds(200)),
                "selection moves the existing rounded capsule with a 200 ms compositor transition while labels remain fixed");
            observations.Add(new { Page = "appearance", Columns = palette.ColumnDefinitions.Count, PaletteWidth = palette.ActualWidth,
                CardSizes = cards.Select(card => new { card.ActualWidth, card.ActualHeight }),
                CapsuleBefore = new { beforeOffset.X, beforeOffset.Y },
                CapsuleAfter = new { capsule.Offset.X, capsule.Offset.Y }, DurationMs = animation?.Duration.TotalMilliseconds });
            await CaptureSettingsWorkspaceAsync(settings, "shell-settings-workspace-appearance.png");
            mode.SelectedKey = originalMode;

            var memory = await SelectAsync("memory");
            await WaitAsync(() => _gateway.MemoryReads == 1 && NativeUi.ById<Button>(memory, "RefreshMemoryButton").IsEnabled,
                "the embedded memory page reads its selected synthetic scope once");
            var memoryKinds = NativeUi.ById<SegmentedSelector>(memory, "MemoryKindPicker");
            var memoryController = ShellField<MemoryManagementWindow>("_memoryManagementWindow");
            var memoryViewModel = (MemoryManagementViewModel)typeof(MemoryManagementWindow)
                .GetField("_viewModel", PrivateInstance)!.GetValue(memoryController)!;
            string originalKind = memoryViewModel.EditorKind;
            Check(NativeUi.Descendants<RadioButton>(memoryKinds).Select(button => button.Tag)
                    .SequenceEqual(new object[] { "fact", "preference", "decision" }) && memoryKinds.SelectedKey == originalKind,
                "memory kind exposes the three original wire values and reflects the loaded editor kind");
            try
            {
                foreach (string kind in new[] { "fact", "preference", "decision" })
                {
                    memoryKinds.SelectedKey = kind;
                    Check(memoryViewModel.EditorKind == kind && _gateway.Writes == 0,
                        "the " + kind + " memory segment updates only the editor with its unchanged wire value");
                }
            }
            finally { memoryKinds.SelectedKey = originalKind; }
            var memorySearch = NativeUi.ById<TextBox>(memory, "MemorySearchBox");
            NativeUi.SetText(memorySearch, "记忆筛选保留");
            var localModels = await SelectAsync("local-models");
            await WaitAsync(() => _gateway.LocalModelReads == 1 && NativeUi.ById<ListView>(localModels, "LocalModelsList").Items.Count == 2,
                "local models display the two fixture rows without starting inference or downloads");
            var loadPolicy = NativeUi.ById<SegmentedSelector>(localModels, "LocalModelsLoadPolicy");
            Check(NativeUi.Descendants<RadioButton>(loadPolicy).Select(button => button.Tag)
                    .SequenceEqual(new object[] { "auto", "on-demand", "preload" }) && loadPolicy.SelectedKey == "on-demand" &&
                _gateway.Writes == 0,
                "local model policy shows three original wire values and reads the synthetic on-demand policy without a settings write");
            var filter = NativeUi.ById<SegmentedSelector>(localModels, "LocalModelsFilter");
            var models = NativeUi.ById<ListView>(localModels, "LocalModelsList");
            Check(NativeUi.Descendants<RadioButton>(filter).Count() == 3,
                "local model filters use the same three-segment rounded selector");
            filter.SelectedKey = "installed";
            Check(models.Items.Count == 1, "installed filtering changes only the actual local model list membership");
            filter.SelectedKey = "downloading";
            Check(models.Items.Count == 0, "download filtering correctly shows an empty list when no transfer exists");
            filter.SelectedKey = "all";
            var modelSearch = NativeUi.ById<TextBox>(localModels, "LocalModelsSearch");
            NativeUi.SetText(modelSearch, "Fixture Local 3B");
            await WaitAsync(() => modelSearch.Text == "Fixture Local 3B" && models.Items.Count == 1,
                "the native text automation reaches the search box and its production TextChanged filter");
            Check(models.Items.Count == 1, "model search stays within cached fixture rows");
            await CaptureSettingsWorkspaceAsync(settings, "shell-settings-workspace-local-models.png");
            await SelectAsync("storage");
            var tools = await SelectAsync("tools");
            await WaitAsync(() => _gateway.AgentReads == 4, "embedded tool management finishes its four read-only configuration requests");
            var retrieval = await SelectAsync("retrieval");
            await WaitAsync(() => _gateway.RetrievalReads == 4, "embedded retrieval settings finish their four synthetic reads");
            string layoutLanguage = UiText.Language;
            try
            {
                foreach (string languageId in new[] { "zh-CN", "en" })
                {
                    UiText.Initialize(languageId);
                    foreach (int width in new[] { 700, 1240 })
                    {
                        await ResizeSettingsAsync(width, 760);
                        root.UpdateLayout();
                        await SettleAsync();
                        CheckNavigationLabels(languageId + " " + width + " DIP retrieval");
                        var retrievalScroll = ((Grid)retrieval).Children.OfType<ScrollViewer>().Single();
                        var controls = new[] { "RetrievalLocalMode", "RetrievalSemanticMode", "RetrievalRerankMode",
                            "RetrievalAddFile", "RetrievalAddFolder", "RetrievalAddLinks", "RetrievalWebMode",
                            "RetrievalWebProvider", "RetrievalWebDepth", "RetrievalWebLanguage", "RetrievalRefresh" }
                            .Select(id => NativeUi.ById<FrameworkElement>(retrieval, id)).Select(control => new
                            {
                                Id = AutomationProperties.GetAutomationId(control), Bounds = BoundsIn(control, retrieval),
                                control.ActualWidth, control.DesiredSize.Width
                            }).ToArray();
                        observations.Add(new { Page = "retrieval", Language = languageId, RequestedWidth = width,
                            RootWidth = retrieval.ActualWidth, retrievalScroll.ViewportWidth,
                            retrievalScroll.ScrollableWidth, Controls = controls });
                        Check(retrievalScroll.ScrollableWidth <= 1 && controls.All(control =>
                            control.Bounds.Width > 0 && control.Bounds.Left >= 23 && control.Bounds.Right <= retrieval.ActualWidth - 23),
                            languageId + " " + width + " DIP retrieval settings keep source actions and every picker inside the page without horizontal clipping");
                        if (languageId == "en")
                            await CaptureSettingsWorkspaceAsync(settings, "shell-settings-workspace-retrieval-" + width + ".png");
                    }
                }
            }
            finally
            {
                UiText.Initialize(layoutLanguage);
                presenter.Restore();
                settings.AppWindow.MoveAndResize(initialBounds);
                await SettleAsync();
            }
            await SelectAsync("about");
            Check(host.Children.Count == keys.Length, "each requested category creates one page and retains it in the same host");
            Check(ReferenceEquals(appearance, await SelectAsync("appearance")) &&
                ReferenceEquals(memory, await SelectAsync("memory")) && memorySearch.Text == "记忆筛选保留" && _gateway.MemoryReads == 1 &&
                ReferenceEquals(localModels, await SelectAsync("local-models")) && models.Items.Count == 1 && _gateway.LocalModelReads == 1 &&
                ReferenceEquals(tools, await SelectAsync("tools")) && _gateway.AgentReads == 4 &&
                ReferenceEquals(retrieval, await SelectAsync("retrieval")) && _gateway.RetrievalReads == 4,
                "returning to categories preserves page identity, search input and completed reads without reloading");
            Check(ReferenceEquals(firstPage, await SelectAsync("general")) && host.Children.Count == keys.Length,
                "the initial general page remains cached after every category has been visited");
            Check(Prompt.Text == draft && ActiveId == chatId && messages == JsonSerializer.Serialize(_shell.ActiveMessages.Select(row => row.Message)) &&
                _gateway.Writes == 0 && _gateway.StreamRequests == 0,
                "settings navigation leaves chat identity, composer text, messages and gateway data untouched");

            Guid? ToolConversationId(ToolManagementWindow controller) =>
                (Guid?)typeof(ToolManagementWindow).GetField("_conversationId", PrivateInstance)!.GetValue(controller);
            Window.Activate();
            await SearchAndSelectAsync(_gateway.WorkChatId, "Work fixture");
            await SelectAsync("tools");
            await WaitAsync(() => ToolConversationId(ShellField<ToolManagementWindow>("_toolManagementWindow")) == _gateway.WorkChatId &&
                !ShellField<ToolManagementWindow>("_toolManagementWindow").HasPendingChanges,
                "the existing search entry binds the embedded tools page to the selected work conversation");
            var workController = ShellField<ToolManagementWindow>("_toolManagementWindow");
            int readsBeforeChatSwitch = _gateway.AgentReads;
            Window.Activate();
            await SearchAndSelectAsync(_gateway.StandaloneChatId, "Chat fixture");
            await SelectAsync("tools");
            await WaitAsync(() => ToolConversationId(ShellField<ToolManagementWindow>("_toolManagementWindow")) == _gateway.StandaloneChatId &&
                !ShellField<ToolManagementWindow>("_toolManagementWindow").HasPendingChanges &&
                _gateway.AgentReads == readsBeforeChatSwitch + 4,
                "switching to the standalone conversation refreshes the clean tool context with four read-only requests");
            Check(!ReferenceEquals(workController, ShellField<ToolManagementWindow>("_toolManagementWindow")) &&
                Prompt.Text == "chat original draft" && ActiveId == _gateway.StandaloneChatId &&
                _gateway.Writes == 0 && _gateway.StreamRequests == 0 && VisibleWindows().SetEquals(originalWindows),
                "tools follow the new chat without reusing stale context, changing its draft or opening another window");
            File.WriteAllText(Path.Combine(_directory, "shell-settings-workspace-observation.json"),
                JsonSerializer.Serialize(observations, new JsonSerializerOptions { WriteIndented = true }));
        }
        catch
        {
            File.WriteAllText(Path.Combine(_directory, "shell-settings-workspace-observation.json"),
                JsonSerializer.Serialize(observations, new JsonSerializerOptions { WriteIndented = true }));
            if (settings is not null)
            {
                try { await NativeWindowCapture.CaptureAsync(settings, Path.Combine(_directory, "shell-settings-workspace-failure.png")); }
                catch { }
            }
            throw;
        }
        finally
        {
            settings?.Close();
            _gateway.EnableSettingsReads = false;
        }
    }

    private async Task CaptureSettingsWorkspaceAsync(Window settings, string filename)
    {
        await Task.Delay(250);
        settings.Activate();
        await SettleAsync();
        int result = FlushDesktopComposition();
        if (result < 0) throw new InvalidOperationException($"DwmFlush failed: 0x{result:X8}");
        await NativeWindowCapture.CaptureAsync(settings, Path.Combine(_directory, filename));
    }
}
