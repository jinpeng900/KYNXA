using System.Reflection;
using System.Runtime.InteropServices;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Layout;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.Views;
using MemoryUiSmoke;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Automation.Provider;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using ShellMigrationUiSmoke;
using Windows.Foundation;

namespace KYNXA_Desktop;

/// <summary>
/// Hosts the production MainWindow/ShellPage with isolated storage and a loopback-only gateway.
/// 用隔离存储与仅回环的模拟网关承载生产 MainWindow/ShellPage。
/// </summary>
public partial class App : Application
{
    public static Window Window { get; private set; } = null!;
    private readonly string _directory = Path.Combine(Path.GetTempPath(), "kynxa-shell-migration-" + Guid.NewGuid().ToString("N"));
    private readonly List<string> _checks = [];
    private readonly ShellGatewayFixture _gateway;
    private ShellPage _shell = null!;
    private Exception? _unhandled;
    private string DataDirectory => Path.Combine(_directory, "Data");
    private string DesktopDirectory => Path.Combine(DataDirectory, "Desktop");
    private string WebViewDirectory => Path.Combine(_directory, "WebView");
    private string ResultPath => Path.Combine(_directory, "result.txt");
    private const BindingFlags PrivateInstance = BindingFlags.Instance | BindingFlags.NonPublic;

    public App()
    {
        Directory.CreateDirectory(DesktopDirectory);
        string workspace = Path.Combine(_directory, "Workspace");
        Directory.CreateDirectory(workspace);
        Environment.SetEnvironmentVariable("KYNXA_DATA_HOME", DataDirectory);
        Environment.SetEnvironmentVariable("KYNXA_DATA_ROOT", DataDirectory);
        Environment.SetEnvironmentVariable("KYNXA_MODEL_HOME", Path.Combine(DataDirectory, "Models"));
        Environment.SetEnvironmentVariable("KYNXA_EXTENSION_HOME", Path.Combine(_directory, "Extensions"));
        Environment.SetEnvironmentVariable("KYNXA_EXTENSION_POINTER", Path.Combine(_directory, "extension-pointer.json"));
        Environment.SetEnvironmentVariable("WEBVIEW2_USER_DATA_FOLDER", WebViewDirectory);
        _gateway = new ShellGatewayFixture(workspace);
        _gateway.HoldInitialCatalog();
        Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", _gateway.Address);
        // Seed UI preferences before the production page can fall back to any legacy settings.
        // 在生产页面可能读取旧设置之前写入临时 UI 偏好。
        File.WriteAllText(Path.Combine(DesktopDirectory, "layout.json"), JsonSerializer.Serialize(new LayoutState
        { SidebarWidth = 318, SidebarCollapsed = false, LastPrimaryContent = "work" }));
        new ModelSelectionStore(DesktopDirectory).Save(new("fixture-provider", "Fixture provider", "fixture-model"));
        File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-shell-migration-latest.txt"), ResultPath);
        InitializeComponent();
        UnhandledException += (_, args) =>
        {
            _unhandled = args.Exception;
            args.Handled = true;
            File.AppendAllText(ResultPath, "\nUNHANDLED: " + args.Exception);
        };
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        File.WriteAllText(ResultPath, "RUNNING production MainWindow/ShellPage with temporary storage, mock HTTP and isolated WebView\n");
        UiText.Initialize("zh-CN");
        Window = new MainWindow();
        Window.Activate();
        _ = RunAsync();
    }

    private T Element<T>(string name) where T : FrameworkElement =>
        _shell.FindName(name) as T ?? NativeUi.ByName<T>(_shell, name);
    private TextBox Prompt => Element<TextBox>("PromptTextBox");
    private Guid? ActiveId => (Guid?)typeof(ShellPage).GetProperty("ActiveChatId", PrivateInstance)!.GetValue(_shell);
    private object? Call(string method, params object?[] arguments) =>
        typeof(ShellPage).GetMethod(method, PrivateInstance)!.Invoke(_shell, arguments);
    private Task<bool> RefreshModelsAsync() => (Task<bool>)Call("RefreshModelPickerAsync")!;
    private string Markdown() => (string)Call("CurrentConversationMarkdown")!;

    private void Check(bool condition, string description)
    {
        if (!condition) throw new InvalidOperationException(description);
        _checks.Add("PASS " + description);
    }

    private async Task WaitAsync(Func<bool> condition, string description)
    {
        DateTime deadline = DateTime.UtcNow.AddSeconds(20);
        while (!condition())
        {
            if (_unhandled is not null) throw new InvalidOperationException("Unhandled production Shell error", _unhandled);
            if (_gateway.Failure is not null) throw new InvalidOperationException("Unexpected mock gateway activity", _gateway.Failure);
            if (DateTime.UtcNow > deadline) throw new TimeoutException(description);
            await Task.Delay(30);
        }
    }

    private static Task SettleAsync() => Task.Delay(220);

    private T Popup<T>(string id) where T : FrameworkElement => VisualTreeHelper.GetOpenPopupsForXamlRoot(_shell.XamlRoot)
        .Where(popup => popup.IsOpen && popup.Child is not null)
        .SelectMany(popup => NativeUi.Descendants<T>(popup.Child!))
        .Single(element => AutomationProperties.GetAutomationId(element) == id);

    private bool HasPopup<T>(string id) where T : FrameworkElement => VisualTreeHelper.GetOpenPopupsForXamlRoot(_shell.XamlRoot)
        .Any(popup => popup.IsOpen && popup.Child is not null && NativeUi.Descendants<T>(popup.Child)
            .Any(element => AutomationProperties.GetAutomationId(element) == id));

    private static ModelChoice ModelChoiceOf(object row) => row is ModelChoice choice ? choice :
        row.GetType().GetProperty("Choice")?.GetValue(row) as ModelChoice
        ?? throw new InvalidOperationException("The production model row has no original model choice.");

    private FrameworkElement PopupRoot(FrameworkElement control) => (FrameworkElement)VisualTreeHelper.GetOpenPopupsForXamlRoot(_shell.XamlRoot)
        .Single(popup => popup.IsOpen && popup.Child is not null && NativeUi.Descendants<FrameworkElement>(popup.Child)
            .Any(element => ReferenceEquals(element, control))).Child;

    private Rect PopupBounds(FrameworkElement control)
    {
        var root = PopupRoot(control);
        return control.TransformToVisual(root).TransformBounds(new Rect(0, 0, control.ActualWidth, control.ActualHeight));
    }

    private bool IsInsidePopup(FrameworkElement control)
    {
        if (!control.IsLoaded || !NativeUi.IsVisible(control)) return false;
        var root = PopupRoot(control);
        var bounds = PopupBounds(control);
        return bounds.Left >= -1 && bounds.Top >= -1 && bounds.Right <= root.ActualWidth + 1 && bounds.Bottom <= root.ActualHeight + 1;
    }

    private async Task CapturePresentedAsync(string filename)
    {
        // Let popup transitions finish, then observe real rendering and desktop composition before capture.
        // 等待弹出动画结束，再观察真实渲染帧及桌面合成后截图。
        await Task.Delay(350);
        var rendered = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        EventHandler<RenderedEventArgs> renderedHandler = (_, _) => rendered.TrySetResult();
        CompositionTarget.Rendered += renderedHandler;
        double originalOpacity = _shell.Opacity;
        try
        {
            // Idle pages do not necessarily render another frame after UpdateLayout alone.
            // 空闲页面仅调用 UpdateLayout 不一定产生新帧。
            _shell.Opacity = originalOpacity == 1 ? 0.999 : 1;
            await rendered.Task.WaitAsync(TimeSpan.FromSeconds(5));
            rendered = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            _shell.Opacity = originalOpacity;
            await rendered.Task.WaitAsync(TimeSpan.FromSeconds(5));
            int result = FlushDesktopComposition();
            if (result < 0) throw new InvalidOperationException($"DwmFlush failed: 0x{result:X8}");
            await NativeWindowCapture.CaptureAsync(Window, Path.Combine(_directory, filename));
        }
        finally
        {
            _shell.Opacity = originalOpacity;
            CompositionTarget.Rendered -= renderedHandler;
        }
    }

    [DllImport("dwmapi.dll", EntryPoint = "DwmFlush")]
    private static extern int FlushDesktopComposition();

    private async Task SearchAndSelectAsync(Guid chatId, string query, string? screenshot = null)
    {
        if (!NativeUi.IsVisible(Element<Button>("HistorySearchButton")))
        {
            await WaitAsync(() => NativeUi.IsVisible(Element<Button>("CompactSidebarButton")),
                "a visible compact sidebar button is available before searching");
            NativeUi.Invoke(Element<Button>("CompactSidebarButton"));
            await WaitAsync(() => NativeUi.IsVisible(Element<Button>("HistorySearchButton")),
                "opening the drawer realizes the visible production search entry");
        }
        NativeUi.Invoke(Element<Button>("HistorySearchButton"));
        await WaitAsync(() => VisualTreeHelper.GetOpenPopupsForXamlRoot(_shell.XamlRoot).Any(popup => popup.IsOpen &&
            popup.Child is not null && NativeUi.Descendants<TextBox>(popup.Child).Any(text => AutomationProperties.GetAutomationId(text) == "HistorySearchBox")),
            "production history search opens");
        var search = Popup<TextBox>("HistorySearchBox");
        var results = Popup<ListView>("HistorySearchResults");
        NativeUi.SetText(search, "NO_MATCH_SYNTHETIC_QUERY");
        await SettleAsync();
        Check(results.Items.Count == 0 && _gateway.Writes == 0, "no-match search does not alter the formal catalog");
        NativeUi.SetText(search, query);
        await SettleAsync();
        var match = results.Items.Single(item => (Guid)item.GetType().GetProperty("Id")!.GetValue(item)! == chatId);
        results.ScrollIntoView(match);
        results.UpdateLayout();
        await WaitAsync(() => results.ContainerFromItem(match) is FrameworkElement,
            "the production history result has a realized native row");
        await SettleAsync();
        var row = (ListViewItem)results.ContainerFromItem(match);
        string title = (string)match.GetType().GetProperty("Title")!.GetValue(match)!;
        string scope = (string)match.GetType().GetProperty("Scope")!.GetValue(match)!;
        Check(NativeUi.Descendants<TextBlock>(row).Any(text => text.Text == title && NativeUi.IsVisible(text)) &&
            NativeUi.Descendants<TextBlock>(row).Any(text => text.Text == scope && NativeUi.IsVisible(text)),
            "the production search template renders the real conversation title and scope, not just populated row objects");
        // Measure the real second line against its native row instead of assuming a theme height.
        // 用真实第二行与原生结果行的边界比较，避免假设主题规定的高度。
        var scopeText = NativeUi.Descendants<TextBlock>(row).Single(text => text.Text == scope && NativeUi.IsVisible(text));
        var scopeBounds = scopeText.TransformToVisual(row).TransformBounds(new Rect(0, 0, scopeText.ActualWidth, scopeText.ActualHeight));
        Check(scopeBounds.Width > 0 && scopeBounds.Height > 0 && scopeBounds.Left >= -0.5 && scopeBounds.Top >= -0.5 &&
            scopeBounds.Right <= row.ActualWidth + 0.5 && scopeBounds.Bottom <= row.ActualHeight + 0.5,
            "the complete scope TextBlock fits inside the actual production search ListViewItem without vertical or horizontal clipping");
        if (screenshot is not null) await CapturePresentedAsync(screenshot);
        await NativeUi.InvokeListItemAsync(results, match);
        await WaitAsync(() => ActiveId == chatId, "search selects the requested stable conversation identity");
        await SettleAsync();
    }

    private async Task CheckModelPickerAsync()
    {
        _gateway.FailModelReads = true;
        int readsBeforeOpening = _gateway.ModelReads;
        NativeUi.Invoke(Element<Button>("ModelPickerButton"));
        await WaitAsync(() => HasPopup<TextBox>("ModelPickerSearchBox") && HasPopup<Button>("RetryModelCatalogButton"),
            "the actual model picker opens with retry after its failed GET refresh");
        var search = Popup<TextBox>("ModelPickerSearchBox");
        var models = Popup<ListView>("ModelPickerList");
        var configure = Popup<Button>("ConfigureCustomModelsButton");
        var retry = Popup<Button>("RetryModelCatalogButton");
        await WaitAsync(() => models.Items.Count == 3 && IsInsidePopup(configure) && IsInsidePopup(retry),
            "cached model choices and both footer actions have native layout");
        await SettleAsync();
        Check(_gateway.ModelReads > readsBeforeOpening && NativeUi.Descendants<TextBlock>(PopupRoot(search))
            .Any(text => text.Text == UiText.Get("下面保留上次读取的模型。") && NativeUi.IsVisible(text)),
            "failed refresh keeps the last successful model list and visibly explains the cached results");
        double footerTop = PopupBounds(configure).Top;
        double openingHeight = PopupRoot(search).ActualHeight;

        NativeUi.SetText(search, "fixture-provider");
        await WaitAsync(() => models.Items.Count == 2, "model picker filters by the stable provider ID");
        Check(models.Items.Cast<object>().All(row => ModelChoiceOf(row).ProviderId == "fixture-provider"),
            "actual model search matches provider ID independently of its display name");
        NativeUi.SetText(search, "fixture-secondary");
        await WaitAsync(() => models.Items.Count == 1, "model picker filters by the exact model ID");
        Check(ModelChoiceOf(models.Items.Cast<object>().Single()).ModelId == "fixture-secondary",
            "actual model search selects the matching model ID within a provider");
        NativeUi.SetText(search, "NO_MATCH_SYNTHETIC_MODEL");
        await WaitAsync(() => models.Items.Count == 0 && NativeUi.Descendants<TextBlock>(PopupRoot(search)).Any(text =>
            text.Text == UiText.Get("没有匹配的模型，请换个关键词或清除搜索。") && NativeUi.IsVisible(text)),
            "model picker presents its real empty search state");
        Check(NativeUi.Descendants<TextBlock>(PopupRoot(search)).Any(text =>
            text.Text == UiText.Get("没有匹配的模型，请换个关键词或清除搜索。") && NativeUi.IsVisible(text)) &&
            IsInsidePopup(configure) && IsInsidePopup(retry) && Math.Abs(PopupBounds(configure).Top - footerTop) < 1,
            "no-match model search renders its explanation while fixed configure and retry actions stay inside the popup viewport");
        Check(Math.Abs(PopupRoot(search).ActualHeight - openingHeight) < 1,
            "filtering models keeps the popup height stable instead of moving its anchor");

        _gateway.FailModelReads = false;
        int readsBeforeRetry = _gateway.ModelReads;
        NativeUi.Invoke(retry);
        await WaitAsync(() => HasPopup<TextBox>("ModelPickerSearchBox") &&
            !ReferenceEquals(Popup<TextBox>("ModelPickerSearchBox"), search) && !HasPopup<Button>("RetryModelCatalogButton"),
            "the actual retry action reopens the successfully refreshed model menu");
        search = Popup<TextBox>("ModelPickerSearchBox");
        models = Popup<ListView>("ModelPickerList");
        configure = Popup<Button>("ConfigureCustomModelsButton");
        await WaitAsync(() => models.Items.Count == 3 && IsInsidePopup(configure),
            "the retried production model menu has presented choices and its fixed configuration footer");
        Check(_gateway.ModelReads > readsBeforeRetry && models.Items.Count == 3 && IsInsidePopup(configure) &&
            AutomationProperties.GetName(Element<Button>("ModelStatusButton")).Contains("已选择模型"),
            "native model retry restores authoritative choices and the configured-model status");
        NativeUi.SetText(search, "fixture-model");
        await WaitAsync(() => models.Items.Count == 2, "duplicate model IDs remain selectable across different providers");
        var target = models.Items.Cast<object>().Single(row => ModelChoiceOf(row) is { ProviderId: "second-provider", ModelId: "fixture-model" });
        models.ScrollIntoView(target);
        models.UpdateLayout();
        await WaitAsync(() => models.ContainerFromItem(target) is FrameworkElement, "the target model has a realized native row");
        await SettleAsync();
        var row = (FrameworkElement)models.ContainerFromItem(target);
        Check(NativeUi.Descendants<TextBlock>(row).Any(text => text.Text == "Second fixture provider" && NativeUi.IsVisible(text)) &&
            NativeUi.Descendants<TextBlock>(row).Any(text => text.Text == "fixture-model" && NativeUi.IsVisible(text)) && IsInsidePopup(configure),
            "the production model template shows provider and model identity with the configuration footer still visible");
        Check(NativeUi.Descendants<TextBlock>(row).Count(text => text.Text == "fixture-model" && NativeUi.IsVisible(text)) == 1,
            "the model template avoids repeating the model ID when it is already the display name");
        Check(PopupRoot(search).ActualHeight < 330,
            "the initial three-model menu uses its content height instead of a 400 DIP blank panel");
        var currentRow = models.Items.Cast<object>().Single(item => ModelChoiceOf(item).ProviderId == "fixture-provider");
        models.ScrollIntoView(currentRow); models.UpdateLayout();
        await SettleAsync();
        var currentContainer = (FrameworkElement)models.ContainerFromItem(currentRow);
        var checkmark = NativeUi.Descendants<FontIcon>(currentContainer).Single(icon => icon.Glyph == "\uE73E");
        Check(NativeUi.IsVisible(checkmark) && IsInsidePopup(checkmark) && PopupBounds(checkmark).Right <= PopupBounds(currentContainer).Right + 1,
            "the current model checkmark is visible and fits inside the actual popup row");
        await CapturePresentedAsync("shell-migration-model-menu.png");
        NativeUi.SetText(search, "second-provider");
        await WaitAsync(() => models.Items.Count == 1, "the second stable provider ID isolates the duplicate model");
        await NativeUi.InvokeListItemAsync(models, target);
        await WaitAsync(() => !HasPopup<TextBox>("ModelPickerSearchBox") &&
            new ModelSelectionStore(DesktopDirectory).Load() is { ProviderId: "second-provider", ModelId: "fixture-model" },
            "native model selection persists the provider and model identity pair in temporary UI preferences");
        Check(typeof(ShellPage).GetField("_selectedModel", PrivateInstance)!.GetValue(_shell) is ModelChoice
            { ProviderId: "second-provider", ModelId: "fixture-model" } && _gateway.Writes == 0 && _gateway.Catalog.Revision == 7,
            "choosing a duplicate model ID preserves stable provider identity without model requests or catalog writes");
    }

    private async Task CheckProjectActionButtonRecoveryAsync()
    {
        string originalDraft = Prompt.Text;
        Guid? originalId = ActiveId;
        NativeUi.SetText(Prompt, "Pending project action synthetic draft");
        await WaitAsync(() => Element<Button>("SendButton").IsEnabled, "a nonempty draft enables the composer before a project action");
        var entered = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var release = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        Task action = (Task)Call("RunProjectActionAsync", (Func<Task>)(async () =>
        {
            entered.TrySetResult();
            await release.Task;
        }))!;
        try
        {
            await entered.Task.WaitAsync(TimeSpan.FromSeconds(5));
            Check(!Element<Button>("SendButton").IsEnabled && !action.IsCompleted,
                "a real pending production project action disables Send before its delegate completes");
            NativeUi.SetText(Prompt, "Edited while the synthetic project action is pending");
            Check(!Element<Button>("SendButton").IsEnabled,
                "editing a nonempty draft during a pending project action keeps Send disabled");
        }
        finally
        {
            release.TrySetResult();
            await action;
        }
        await WaitAsync(() => Element<Button>("SendButton").IsEnabled, "Send restores after the real project action completes");
        Check(ActiveId == originalId && _gateway.Writes == 0,
            "project action completion restores Send while preserving conversation identity and avoiding gateway writes");
        NativeUi.SetText(Prompt, originalDraft);
    }

    private void ResizeInDips(int width, int height)
    {
        double scale = _shell.XamlRoot.RasterizationScale;
        Window.AppWindow.Resize(new Windows.Graphics.SizeInt32((int)Math.Ceiling(width * scale), (int)Math.Ceiling(height * scale)));
    }

    private async Task CheckNavigationMenuAsync()
    {
        var more = NativeUi.Descendants<Button>((FrameworkElement)Window.Content)
            .Single(button => AutomationProperties.GetName(button) == UiText.Get("更多功能"));
        NativeUi.Invoke(more);
        await WaitAsync(() => VisualTreeHelper.GetOpenPopupsForXamlRoot(_shell.XamlRoot).Any(popup => popup.IsOpen && popup.Child is not null &&
            NativeUi.Descendants<MenuFlyoutItem>(popup.Child).Any(item => item.Text == UiText.Get("聚焦输入 Ctrl+L"))),
            "the production title-bar more button opens its navigation menu");
        var items = VisualTreeHelper.GetOpenPopupsForXamlRoot(_shell.XamlRoot).Where(popup => popup.IsOpen && popup.Child is not null)
            .SelectMany(popup => NativeUi.Descendants<MenuFlyoutItem>(popup.Child!)).ToArray();
        Check(items.Any(item => item.Text == UiText.Get("搜索会话 Ctrl+F")) && items.Any(item => item.Text == UiText.Get("工具与技能")),
            "the real navigation menu exposes conversation search and the existing tool management entry");
        var focus = items.Single(item => item.Text == UiText.Get("聚焦输入 Ctrl+L"));
        var peer = FrameworkElementAutomationPeer.CreatePeerForElement(focus);
        if (peer?.GetPattern(PatternInterface.Invoke) is not IInvokeProvider invoke)
            throw new InvalidOperationException("The production navigation item does not expose native Invoke automation.");
        invoke.Invoke();
        await SettleAsync();
        Check(Prompt.FocusState != FocusState.Unfocused, "the production navigation menu can focus the composer");
    }

    private async Task CheckModalGuardAsync()
    {
        var tool = new ToolActivity("fixture-approval", "workspace.read_file", null, "approval-required", "Read the synthetic mounted file",
            ApprovalId: Guid.NewGuid(), WorkspaceRoot: Path.Combine(_directory, "Workspace"));
        var dialog = ToolApprovalDialog.Create(_shell.XamlRoot, tool);
        var showing = dialog.ShowAsync();
        await WaitAsync(() => NativeUi.OpenDialog(_shell) is not null, "production approval dialog occupies the real Shell XamlRoot");
        Check((bool)Call("HasPresentationModal")!, "modal guard sees a production approval ContentDialog, not only feature-help dialogs");
        Guid? originalId = ActiveId;
        string originalDraft = Prompt.Text;
        int retrievalReads = _gateway.RetrievalReads;
        object? focus = FocusManager.GetFocusedElement(_shell.XamlRoot);
        // Null event arguments are safe only if the real handlers return at their modal guard.
        // 只有真实处理函数在模态守卫处返回时，空事件参数才是安全的。
        Call("NewChatShortcut_Invoked", null, null);
        Call("SearchShortcut_Invoked", null, null);
        Call("ComposerShortcut_Invoked", null, null);
        Call("KnowledgeNavigation_Click", null, null);
        _shell.ShowNavigationMenu(Element<Button>("HistorySearchButton"));
        await SettleAsync();
        Check(ActiveId == originalId && Prompt.Text == originalDraft && ReferenceEquals(FocusManager.GetFocusedElement(_shell.XamlRoot), focus) &&
            CurrentRetrievalWindow is null && _gateway.RetrievalReads == retrievalReads &&
            VisualTreeHelper.GetOpenPopupsForXamlRoot(_shell.XamlRoot).Where(popup => popup.IsOpen && popup.Child is not null)
                .All(popup => !NativeUi.Descendants<TextBox>(popup.Child!).Any(text => AutomationProperties.GetAutomationId(text) == "HistorySearchBox")),
            "production new/search/composer shortcuts, knowledge entry and navigation menu leave approval focus, identity and draft intact");
        NativeUi.InvokeDialogButton(dialog, primary: false);
        await showing;
        await SettleAsync();
        Check(!(bool)Call("HasPresentationModal")!, "modal guard releases after closing the approval dialog");
    }

    private async Task RunAsync()
    {
        try
        {
            await WaitAsync(() => NativeUi.Descendants<Frame>((FrameworkElement)Window.Content).FirstOrDefault()?.Content is ShellPage,
                "production MainWindow creates production ShellPage");
            _shell = (ShellPage)NativeUi.Descendants<Frame>((FrameworkElement)Window.Content).First().Content;
            await WaitAsync(() => _shell.IsLoaded && _shell.XamlRoot is not null,
                "the production Shell is loaded before using its actual DPI");
            // MainWindow's initial size is physical pixels; request DIP dimensions after XamlRoot exists.
            // MainWindow 初始尺寸是物理像素；XamlRoot 可用后按实际 DPI 请求 DIP 尺寸。
            ResizeInDips(1440, 900);
            await WaitAsync(() => Element<Grid>("ShellGrid").ActualWidth >= 980 && NativeUi.IsVisible(Element<Button>("HistorySearchButton")),
                "the initialized fixture has a realized wide sidebar before search automation");
            await CheckInitialHomeAsync();
            await WaitAsync(() => _shell.IsLoaded && _shell.XamlRoot is not null && _gateway.CatalogReads == 1 &&
                (bool)typeof(ShellPage).GetField("_projectsReady", PrivateInstance)!.GetValue(_shell)! && _gateway.ModelReads >= 1 &&
                AutomationProperties.GetName(Element<Button>("ModelStatusButton")).Contains("已选择模型") &&
                Element<TextBlock>("SelectedModelLabel").Text == "fixture-model", "production Shell initializes from isolated mock catalog and model preference");
            Check(StoragePaths.DesktopDirectory == DesktopDirectory && _gateway.Writes == 0,
                "production storage paths resolve exclusively to the temporary Desktop directory");
            var transcript = Element<ConversationTranscript>("ConversationMessages");
            await transcript.Ready.WaitAsync(TimeSpan.FromSeconds(25));
            Check(string.Equals(Path.TrimEndingDirectorySeparator(transcript.Browser.CoreWebView2.Environment.UserDataFolder),
                Path.TrimEndingDirectorySeparator(WebViewDirectory), StringComparison.OrdinalIgnoreCase),
                "production Transcript WebView uses the fixture's isolated browser data folder");
            string status = AutomationProperties.GetName(Element<Button>("ModelStatusButton"));
            Check(status.Contains("已选择模型") && status.Contains("不代表模型调用已验证"),
                "configured model status truthfully distinguishes selection from a verified provider request");
            Check(!Element<Button>("SendButton").IsEnabled, "empty initialized composer cannot send");
            await CheckEmptyComposerPolishAsync();
            _gateway.FailModelReads = true;
            Check(!await RefreshModelsAsync() && AutomationProperties.GetName(Element<Button>("ModelStatusButton")).Contains("无法读取模型连接") &&
                Element<TextBlock>("SelectedModelLabel").Text == "fixture-model", "failed model refresh reports an error while retaining the last selected model");
            _gateway.FailModelReads = false;
            Check(await RefreshModelsAsync() && AutomationProperties.GetName(Element<Button>("ModelStatusButton")).Contains("已选择模型"),
                "successful model refresh restores the selected-model status");
            await CheckModelPickerAsync();
            await CheckNavigationMenuAsync();

            await SearchAndSelectAsync(_gateway.WorkChatId, "Work fixture");
            Check(!_shell.ViewModel.IsChatMode && Prompt.Text == "work original draft", "search enters the saved work conversation with its own draft");
            NativeUi.SetText(Prompt, "work changed draft");
            await SearchAndSelectAsync(_gateway.StandaloneChatId, "Chat fixture");
            Check(_shell.ViewModel.IsChatMode && Prompt.Text == "chat original draft", "cross-mode search opens the standalone conversation with its own draft");
            NativeUi.SetText(Prompt, "chat changed draft");
            string snapshot = Markdown();
            Check(snapshot.Contains("Chat fixture title") && snapshot.Contains("Final visible answer $x^2$") &&
                !snapshot.Contains("HIDDEN_SUCCESS_STAGE") && !snapshot.Contains("SECRET_") && !snapshot.Contains("HIDDEN_TOOL_RECEIPT"),
                "whole-conversation export uses the production visible projection and preserves raw TeX while omitting hidden successful process content");
            await SearchAndSelectAsync(_gateway.WorkChatId, "Work fixture");
            Check(Prompt.Text == "work changed draft" && ActiveId == _gateway.WorkChatId && snapshot.Contains("Chat fixture title") &&
                !snapshot.Contains("Work synthetic answer") && Markdown().Contains("Work synthetic answer"),
                "returning through search restores the work draft and leaves an earlier conversation export snapshot unchanged");
            await SearchAndSelectAsync(_gateway.StandaloneChatId, "Chat fixture");
            Check(Prompt.Text == "chat changed draft" && _gateway.Writes == 0 && _gateway.Catalog.Revision == 7 &&
                _shell.ActiveMessages.All(row => row.ConversationId == _gateway.StandaloneChatId),
                "returning through search restores the standalone draft without changing catalog revision or message identity");
            await CheckProjectActionButtonRecoveryAsync();

            var storedLayout = new LayoutStateService().Load();
            ResizeInDips(800, 720);
            await WaitAsync(() => Element<Grid>("ShellGrid").ActualWidth is > 0 and < ShellLayoutMetrics.CompactSidebarBreakpoint,
                "the actual native Shell width reaches the requested narrow layout");
            await SettleAsync();
            Check(NativeUi.IsVisible(Element<Button>("CompactSidebarButton")) && !NativeUi.IsVisible(Element<Border>("ChatWorkSwitcher")),
                $"800 DIP Shell shows the compact sidebar affordance (actual width {Element<Grid>("ShellGrid").ActualWidth:F1} DIP)");
            NativeUi.Invoke(Element<Button>("CompactSidebarButton"));
            await SettleAsync();
            Check(NativeUi.IsVisible(Element<Button>("SidebarScrim")) && NativeUi.IsVisible(Element<Button>("HistorySearchButton")),
                "the production compact button opens the drawer over the main content");
            await SearchAndSelectAsync(_gateway.WorkChatId, "Work fixture", "shell-migration-drawer-search-800.png");
            Check(!NativeUi.IsVisible(Element<Button>("SidebarScrim")) && Prompt.Text == "work changed draft",
                "selecting a conversation closes the temporary drawer and preserves its work draft");
            ResizeInDips(1440, 900);
            await WaitAsync(() => Element<Grid>("ShellGrid").ActualWidth >= ShellLayoutMetrics.CompactSidebarBreakpoint,
                "the actual native Shell width reaches the restored wide layout");
            await SettleAsync();
            var widenedLayout = new LayoutStateService().Load();
            Check(widenedLayout.SidebarWidth == storedLayout.SidebarWidth && widenedLayout.SidebarCollapsed == storedLayout.SidebarCollapsed &&
                !NativeUi.IsVisible(Element<Button>("SidebarScrim")), "narrowing and widening never persist temporary drawer width or collapse state");
            var pane = Element<Grid>("SidebarPane");
            Check(Math.Abs(pane.ActualWidth - storedLayout.SidebarWidth) < 2,
                "wide Shell restores the user's saved sidebar width");
            await CheckModalGuardAsync();
            await CapturePresentedAsync("shell-migration-wide.png");
            await CheckComposerStatesPolishAsync();
            await CheckPanelPolishAsync();
            await CheckComprehensiveShellAsync();
            await CheckRetrievalNavigationAsync();
            Check(_gateway.Writes == 0 && _gateway.Failure is null && _unhandled is null,
                "native Shell migration checks complete without formal data writes, upstream model calls or unhandled errors");
            File.AppendAllText(ResultPath, string.Join("\n", _checks) + $"\nPASS: {_checks.Count} production Shell UI migration checks.\nPreviews: {_directory}");
        }
        catch (Exception error)
        {
            Environment.ExitCode = 1;
            File.AppendAllText(ResultPath, string.Join("\n", _checks) + "\nFAIL after " + _checks.Count + " checks: " + error);
            try { await NativeWindowCapture.CaptureAsync(Window, Path.Combine(_directory, "shell-migration-failure.png")); }
            catch (Exception captureError) { File.AppendAllText(ResultPath, "\nFailure screenshot could not be saved: " + captureError.Message); }
        }
        finally
        {
            // Close only this fixture's owned window; programmatic close avoids metadata-save testing here.
            // 只关闭夹具自有窗口；程序关闭绕过本次不涉及的元数据保存验收。
            Window?.Close();
            await _gateway.DisposeAsync();
        }
    }
}
