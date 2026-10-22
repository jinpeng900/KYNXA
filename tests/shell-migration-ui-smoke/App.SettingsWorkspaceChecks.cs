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
                    return new { Key = key, label.TextWrapping, Bounds = BoundsIn(label, button), label.DesiredSize,
                        ButtonWidth = button.ActualWidth, ButtonHeight = button.ActualHeight, button.Padding };
                }).ToArray();
                Check(labels.All(label => label.TextWrapping == TextWrapping.Wrap && label.Bounds.Width > 0 && label.Bounds.Height > 0
                    && label.Bounds.Left >= label.Padding.Left - 1 && label.Bounds.Right <= label.ButtonWidth - label.Padding.Right + 1
                    && label.Bounds.Top >= label.Padding.Top - 1 && label.Bounds.Bottom <= label.ButtonHeight - label.Padding.Bottom + 1
                    && label.Bounds.Width >= label.DesiredSize.Width - 1 && label.Bounds.Height >= label.DesiredSize.Height - 1),
                    state + " navigation keeps all eight localized labels inside the padded buttons without clipping");
            }
            async Task CheckResponsivePagesAsync(string state)
            {
                CheckNavigationLabels(state);
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
                        Controls = measured });
                    Check(Math.Abs(headingBounds.Left - 24) <= 1 && Math.Abs(contentBounds.Left - 24) <= 1 &&
                        contentBounds.Width > 200 && contentBounds.Right <= page.ActualWidth - 23 &&
                        Math.Abs(frame.ActualWidth - page.ActualWidth) <= 1 && page.ScrollableWidth <= 1 &&
                        measured.All(item => item.Bounds.Width > 0 && item.Bounds.Left >= 23 && item.Bounds.Right <= page.ActualWidth - 23),
                        state + " " + key + " aligns heading and content at 24 DIP and keeps cards and actions inside the right viewport");
                    if (page.ScrollableHeight > 1)
                    {
                        page.ChangeView(null, page.ScrollableHeight, null, true);
                        await SettleAsync();
                        Rect bottom = BoundsIn(content, page);
                        Check(Math.Abs(page.VerticalOffset - page.ScrollableHeight) <= 1 && bottom.Bottom <= page.ActualHeight - 23 && bottom.Bottom > 0,
                            state + " " + key + " scrolls the full content bottom into view without clipping lower actions");
                    }
                }
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

            var appearance = await SelectAsync("appearance");
            var mode = NativeUi.ById<SegmentedSelector>(appearance, "AppearanceModePicker");
            var palette = NativeUi.ById<Grid>(appearance, "AppearancePaletteGrid");
            var cards = palette.Children.OfType<Button>().ToArray();
            Check(palette.ColumnDefinitions.Count == 2 && cards.Length == 5 && palette.ActualWidth <= 461 &&
                cards.All(card => card.ActualWidth is > 100 and <= 231 && card.ActualHeight is >= 76 and <= 89 &&
                    card.CornerRadius.TopLeft >= 10) && Grid.GetRow(cards[0]) == Grid.GetRow(cards[1]) &&
                Grid.GetColumn(cards[0]) == 0 && Grid.GetColumn(cards[1]) == 1,
                "the five smaller rounded palette cards align in two columns without stretching across the page");
            Check(NativeUi.Descendants<RadioButton>(mode).Count() == 3 && mode.SelectedKey == originalMode,
                "theme mode uses three real radio segments with the saved selection");
            var capsule = typeof(SegmentedSelector).GetField("_capsule", PrivateInstance)?.GetValue(mode) as ShapeVisual
                ?? throw new InvalidOperationException("The loaded segment has no actual compositor capsule.");
            var beforeOffset = capsule.Offset;
            var labelPositions = NativeUi.Descendants<RadioButton>(mode).Select(button =>
                button.TransformToVisual(mode).TransformPoint(default)).ToArray();
            string nextMode = originalMode == "dark" ? "light" : "dark";
            var target = NativeUi.ById<RadioButton>(mode, "Segment_" + nextMode);
            settings.Activate();
            await WaitAsync(() => GetForegroundWindow() == handle, "the fixture-owned settings window is foreground before native input");
            target.StartBringIntoView();
            await SettleAsync();
            MovePointerInsideSettings(settings, target);
            SendFixtureInput([
                new NativeInput { Data = new NativeInputData { Mouse = new NativeMouseInput { Flags = 0x0002 } } },
                new NativeInput { Data = new NativeInputData { Mouse = new NativeMouseInput { Flags = 0x0004 } } }
            ]);
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
            await WaitAsync(() => _gateway.RetrievalInitialReads == (1, 1, 1) && _gateway.RetrievalStatusReads >= 1,
                "embedded retrieval settings read settings, providers and sources once and start read-only status observation");
            var completedReads = (_gateway.MemoryReads, _gateway.LocalModelReads, _gateway.AgentReads, _gateway.RetrievalInitialReads);
            string CachedState() => $"memorySearch={memorySearch.Text}; modelSearch={modelSearch.Text}; filter={filter.SelectedKey}; " +
                $"modelRows={models.Items.Count}; reads=memory:{_gateway.MemoryReads},local:{_gateway.LocalModelReads},tools:{_gateway.AgentReads}," +
                $"retrievalInitial:{_gateway.RetrievalInitialReads},retrievalStatus:{_gateway.RetrievalStatusReads}";
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
                        var scroll = ((Grid)retrieval).Children.OfType<ScrollViewer>().Single();
                        var bounds = new[] { "RetrievalLocalMode", "RetrievalSemanticMode",
                            "RetrievalAddFile", "RetrievalAddFolder", "RetrievalAddLinks", "RetrievalWebMode",
                            "RetrievalWebProvider", "RetrievalWebDepth", "RetrievalWebLanguage", "RetrievalRefresh" }
                            .Select(id => BoundsIn(NativeUi.ById<FrameworkElement>(retrieval, id), retrieval)).ToArray();
                        Check(scroll.ScrollableWidth <= 1 && bounds.All(item => item.Width > 0
                            && item.Left >= 23 && item.Right <= retrieval.ActualWidth - 23),
                            languageId + " " + width + " DIP retrieval actions and pickers stay inside the viewport");
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
            observations.Add(new { State = "after-localization", CachedState = CachedState() });
            Check((_gateway.MemoryReads, _gateway.LocalModelReads, _gateway.AgentReads, _gateway.RetrievalInitialReads) == completedReads,
                "language and window-size changes retain completed reads without reloading: " + CachedState());
            Check(ReferenceEquals(appearance, await SelectAsync("appearance")), "appearance retains its cached page identity");
            var returnedMemory = await SelectAsync("memory");
            Check(ReferenceEquals(memory, returnedMemory) && ReferenceEquals(memorySearch, NativeUi.ById<TextBox>(returnedMemory, "MemorySearchBox")) &&
                memorySearch.Text == "记忆筛选保留" && _gateway.MemoryReads == 1,
                "memory retains page identity, actual search control, query and completed read: " + CachedState());
            var returnedModels = await SelectAsync("local-models");
            Check(ReferenceEquals(localModels, returnedModels) && ReferenceEquals(modelSearch, NativeUi.ById<TextBox>(returnedModels, "LocalModelsSearch")) &&
                modelSearch.Text == "Fixture Local 3B" && filter.SelectedKey == "all" && models.Items.Count == 1 && _gateway.LocalModelReads == 1,
                "local models retain page identity, actual search control, query, filter and completed read: " + CachedState());
            Check(ReferenceEquals(tools, await SelectAsync("tools")) && _gateway.AgentReads == 4,
                "tools retain page identity and their four completed reads: " + CachedState());
            Check(ReferenceEquals(retrieval, await SelectAsync("retrieval")) && _gateway.RetrievalInitialReads == (1, 1, 1),
                "retrieval retains page identity and each of its three initialization reads while read-only status observation continues: " + CachedState());
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
