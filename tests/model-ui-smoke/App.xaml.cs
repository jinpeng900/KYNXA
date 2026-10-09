using KYNXA_Desktop;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;

namespace ModelUiSmoke;

public partial class App : Application
{
    private readonly string _directory = Path.Combine(Path.GetTempPath(), "kynxa-model-ui-smoke-" + Guid.NewGuid().ToString("N"));
    private readonly List<string> _checks = [];
    private readonly FakeModelGateway _gateway;
    private ModelManagementWindow? _window;
    private Window? _anchor;
    private FrameworkElement _root = null!;
    private Exception? _unhandled;
    private bool _windowClosed;
    private readonly bool _closeDuringInitialization = Environment.GetCommandLineArgs().Contains("--close-during-initialization", StringComparer.Ordinal);
    private string ResultPath => Path.Combine(_directory, "result.txt");

    public App()
    {
        Directory.CreateDirectory(_directory);
        Environment.SetEnvironmentVariable("KYNXA_DATA_HOME", Path.Combine(_directory, "Data"));
        _gateway = new FakeModelGateway();
        if (_closeDuringInitialization) _gateway.ListDelayMs = 750;
        Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", _gateway.Address);
        File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-model-ui-smoke-latest.txt"), ResultPath);
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
        File.WriteAllText(ResultPath, "RUNNING: isolated production model settings and loopback fake gateway\n");
        UiText.Initialize("zh-CN");
        // Match the production application's palette initialization before any native controls are created.
        // 与正式应用保持一致，在创建任何原生控件前初始化配色资源。
        AppearanceService.Apply(AppearanceService.DefaultPaletteId);
        _anchor = new Window { Content = new Grid() };
        _anchor.AppWindow.Hide();
        _window = new ModelManagementWindow();
        _window.Closed += (_, _) => _windowClosed = true;
        _root = (FrameworkElement)_window.Content;
        _window.Activate();
        _ = RunAsync();
    }

    private T Element<T>(string name) where T : FrameworkElement => _root.FindName(name) as T ?? NativeUi.ByName<T>(_root, name);
    private Button Button(string name) => Element<Button>(name);
    private ComboBox OutputChoice => Element<ComboBox>("MaxOutputTokensBox");
    private ComboBox ContextChoice => Element<ComboBox>("ContextWindowBox");
    private TextBox CustomOutput => Element<TextBox>("CustomMaxOutputTokensBox");
    private static string ChoiceValue(ComboBox choice) => (choice.SelectedItem as ComboBoxItem)?.Tag?.ToString() ?? string.Empty;

    private void Check(bool condition, string description)
    {
        if (!condition) throw new InvalidOperationException(description);
        _checks.Add("PASS " + description);
    }

    private async Task WaitAsync(Func<bool> condition, string description)
    {
        var deadline = DateTime.UtcNow.AddSeconds(10);
        while (!condition())
        {
            if (_unhandled is not null) throw new InvalidOperationException("Unhandled native model UI error", _unhandled);
            if (_gateway.Failure is not null) throw new InvalidOperationException("Fake gateway failed", _gateway.Failure);
            if (DateTime.UtcNow > deadline) throw new TimeoutException(description + "; " + ModelInteractionState());
            await Task.Delay(25);
        }
        Check(true, description);
    }

    private static Task SettleAsync() => Task.Delay(180);

    private string ModelInteractionState() => $"widthDip={_root.ActualWidth:F0}; busy={_window?.IsModelOperationBusyForTest}; " +
        $"confirmPending={_window?.IsDiscardConfirmationPendingForTest}; dirty={_window?.HasModelDraftForTest}; " +
        $"editingId={_window?.EditingProviderIdForTest ?? "(new)"}; " +
        $"compactVisible={NativeUi.IsVisible(Element<ComboBox>("CompactConnectionPicker"))}; " +
        $"savedListVisible={NativeUi.IsVisible(Element<Grid>("SavedConnectionsList"))}; " +
        $"dialogVisible={(_root.XamlRoot is not null && NativeUi.OpenDialog(_root) is not null)}; " +
        $"listReads={_gateway.ListReads}; providerRows={Element<StackPanel>("ProviderList").Children.Count}; " +
        $"status={Element<TextBlock>("StatusText").Text}";

    private async Task SetModelIdsAsync(string models)
    {
        var editor = Element<TextBox>("ModelsBox");
        var section = NativeUi.Descendants<Expander>(_root).Single(expander => ReferenceEquals(expander.Content, editor));
        section.IsExpanded = true;
        await SettleAsync();
        NativeUi.SetText(editor, models);
        await SettleAsync();
    }

    private void SelectTokenChoice(ComboBox choice, string value) => choice.SelectedItem = choice.Items.OfType<ComboBoxItem>()
        .Single(item => item.Tag?.ToString() == value);

    private async Task SelectProviderAsync(string name, bool expectDiscard = false, bool discard = true)
    {
        await WaitAsync(() => !_window!.IsDiscardConfirmationPendingForTest && !_window.IsModelOperationBusyForTest,
            "connection selection waits for the preceding native decision to finish");
        if (expectDiscard) Check(_window!.HasModelDraftForTest, "the next connection switch still owns an unsaved draft");
        var compact = Element<ComboBox>("CompactConnectionPicker");
        var target = compact.Items.OfType<ModelProvider>().Single(provider => provider.DisplayName == name);
        File.AppendAllText(ResultPath, "\nBEFORE_SELECT " + ModelInteractionState() + "\n");
        if (NativeUi.IsVisible(compact)) compact.SelectedItem = target;
        else
        {
            var panel = Element<StackPanel>("ProviderList");
            var button = panel.Children.OfType<Button>().Single(item => item.Content is StackPanel content &&
                content.Children.OfType<TextBlock>().First().Text == name);
            // The accepted connection identity can be ready before its newly rebuilt buttons receive layout.
            // 连接身份已接受时，新建的连接按钮可能尚未完成布局；等待实际可见控件再调用自动化。
            await WaitAsync(() => NativeUi.IsVisible(button), "connection automation targets the visible saved-connection button");
            NativeUi.Invoke(button);
        }
        if (expectDiscard) await ResolveDiscardAsync(discard);
        if (!expectDiscard || discard)
        {
            await WaitAsync(() => _window!.EditingProviderIdForTest == target.ProviderId && !_window.IsDiscardConfirmationPendingForTest,
                "native connection selection loads the requested stable provider identity");
            if (!expectDiscard) Check(NativeUi.OpenDialog(_root) is null, "unchanged connection switches without a discard dialog");
        }
    }

    private async Task ResolveDiscardAsync(bool discard)
    {
        await WaitAsync(() => NativeUi.OpenDialog(_root) is not null, "changed form requests an explicit discard decision");
        var dialog = NativeUi.OpenDialog(_root)!;
        Check(dialog.DefaultButton == ContentDialogButton.Close, "discard confirmation defaults to keeping the draft");
        NativeUi.InvokeDialogButton(dialog, primary: discard);
        await WaitAsync(() => NativeUi.OpenDialog(_root) is null, "discard decision closes the native dialog");
        File.AppendAllText(ResultPath, "\nAFTER_POPUP_CLOSE " + ModelInteractionState() + "\n");
        await WaitAsync(() => !_window!.IsDiscardConfirmationPendingForTest,
            "discard decision releases its production confirmation guard before another action");
    }

    private async Task SelectPresetAsync(string presetId, bool expectDiscard)
    {
        Element<ComboBox>("PresetBox").SelectedItem = ModelPresets.All.Single(preset => preset.Id == presetId);
        await SettleAsync();
        if (expectDiscard) await ResolveDiscardAsync(discard: true);
        else Check(NativeUi.OpenDialog(_root) is null, "unchanged preset switches without a discard dialog");
    }

    private void ResizeInDips(int width, int height)
    {
        double scale = _root.XamlRoot.RasterizationScale;
        _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32((int)Math.Ceiling(width * scale),
            (int)Math.Ceiling(height * scale)));
    }

    private async Task VerifyDiscardProtectionAsync()
    {
        int writes = _gateway.Saves;
        var key = Element<PasswordBox>("ApiKeyBox");
        var customContext = Element<TextBox>("CustomContextWindowBox");
        var presets = Element<ComboBox>("PresetBox");
        var previousPreset = presets.SelectedItem;
        SelectTokenChoice(OutputChoice, "8192");
        await SelectProviderAsync("Legacy / 旧连接", expectDiscard: true, discard: false);
        Check(ChoiceValue(OutputChoice) == "8192" && key.Password.Length == 0,
            "an output-only change requests confirmation and survives cancelling without relying on a key edit");
        SelectTokenChoice(OutputChoice, "custom");
        NativeUi.SetText(CustomOutput, "2048");
        SelectTokenChoice(ContextChoice, "custom");
        NativeUi.SetText(customContext, "invalid-token-draft");
        NativeUi.Invoke(Button("NewConnectionButton"));
        await ResolveDiscardAsync(discard: false);
        Check(customContext.Text == "invalid-token-draft" && _gateway.Saves == writes,
            "an invalid context-only draft is protected without parsing or sending a configuration");
        SelectTokenChoice(ContextChoice, "1000000");
        key.Password = "synthetic-unsaved-key";
        NativeUi.RequestClose(_window!);
        await ResolveDiscardAsync(discard: false);
        Check(!_windowClosed && key.Password == "synthetic-unsaved-key",
            "a key-only change protects native close even when the other fields match the saved connection");
        SelectTokenChoice(ContextChoice, "custom");
        NativeUi.SetText(customContext, "65537");
        SelectTokenChoice(OutputChoice, "custom");
        NativeUi.SetText(CustomOutput, "12345");
        key.Password = "synthetic-unsaved-key";
        await SettleAsync();
        await SelectProviderAsync("Legacy / 旧连接", expectDiscard: true, discard: false);
        Check(Element<TextBox>("NameBox").Text == "Small / 两个模型" && ChoiceValue(ContextChoice) == "custom" &&
            customContext.Text == "65537" && CustomOutput.Text == "12345" && key.Password == "synthetic-unsaved-key" &&
            _gateway.Saves == writes, "cancelled connection switch preserves both custom budgets, key and stable connection identity without saving");

        NativeUi.Invoke(Button("NewConnectionButton"));
        await WaitAsync(() => NativeUi.OpenDialog(_root) is not null, "new connection protects the current draft");
        UiText.Initialize("en");
        await SettleAsync();
        Check(NativeUi.OpenDialog(_root)!.Title?.ToString() == "Unsaved changes" && customContext.Text == "65537" &&
            key.Password == "synthetic-unsaved-key", "an open confirmation localizes without resetting draft tokens or the key");
        UiText.Initialize("zh-CN");
        await ResolveDiscardAsync(discard: false);
        presets.SelectedItem = ModelPresets.All.Single(preset => preset.Id == "anthropic");
        await WaitAsync(() => NativeUi.OpenDialog(_root) is not null, "preset switch protects the current draft");
        Check(ReferenceEquals(presets.SelectedItem, previousPreset) && customContext.Text == "65537",
            "the tentative preset selection rolls back while the decision is pending");
        await ResolveDiscardAsync(discard: false);

        NativeUi.RequestClose(_window!);
        await WaitAsync(() => NativeUi.OpenDialog(_root) is not null, "native close protects unsaved model settings");
        await ResolveDiscardAsync(discard: false);
        Check(!_windowClosed && key.Password == "synthetic-unsaved-key", "cancelled native close keeps the window and the key draft");

        ResizeInDips(520, 720);
        await SettleAsync();
        var compactPicker = Element<ComboBox>("CompactConnectionPicker");
        Check(NativeUi.IsVisible(compactPicker) && !NativeUi.IsVisible(Element<Grid>("SavedConnectionsList")),
            "narrow windows replace the fixed connection sidebar with a saved-connection picker");
        compactPicker.SelectedItem = compactPicker.Items.OfType<ModelProvider>().Single(provider => provider.ProviderId == "fixture-legacy");
        await ResolveDiscardAsync(discard: false);
        Check(compactPicker.SelectedItem is ModelProvider { ProviderId: "fixture-small" } && customContext.Text == "65537" &&
            CustomOutput.Text == "12345", "cancelled compact-picker switch restores selection and both custom budgets");
        await BringOutputIntoViewAsync();
        var scroll = Element<ScrollViewer>("EditorScroll");
        var customBounds = CustomOutput.TransformToVisual(scroll).TransformBounds(new Rect(0, 0, CustomOutput.ActualWidth, CustomOutput.ActualHeight));
        var outputBounds = OutputChoice.TransformToVisual(scroll).TransformBounds(new Rect(0, 0, OutputChoice.ActualWidth, OutputChoice.ActualHeight));
        var saveBounds = Button("SaveButton").TransformToVisual(_root).TransformBounds(new Rect(0, 0, Button("SaveButton").ActualWidth, Button("SaveButton").ActualHeight));
        Check(CustomOutput.ActualWidth >= 100 && customBounds.Top >= outputBounds.Bottom && customBounds.Right <= scroll.ActualWidth + 1 &&
            saveBounds.Right <= _root.ActualWidth + 1 && saveBounds.Bottom <= _root.ActualHeight + 1,
            "520 DIP layout stacks custom tokens and keeps the fixed save button inside the window");
        await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "draft-budgets-compact-zh.png"));
        UiText.Initialize("en");
        await SettleAsync();
        var probeBounds = Button("ProbeButton").TransformToVisual(_root).TransformBounds(new Rect(0, 0, Button("ProbeButton").ActualWidth, Button("ProbeButton").ActualHeight));
        saveBounds = Button("SaveButton").TransformToVisual(_root).TransformBounds(new Rect(0, 0, Button("SaveButton").ActualWidth, Button("SaveButton").ActualHeight));
        Check(probeBounds.Left >= 0 && saveBounds.Right <= _root.ActualWidth + 1 && customContext.Text == "65537" &&
            CustomOutput.Text == "12345" && compactPicker.SelectedItem is ModelProvider { ProviderId: "fixture-small" },
            "English compact actions fit without changing the chosen connection or custom token drafts");
        await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "draft-budgets-compact-en.png"));
        UiText.Initialize("zh-CN");
        ResizeInDips(1120, 860);
        await SettleAsync();
        await SelectProviderAsync("Legacy / 旧连接", expectDiscard: true);
        Check(key.Password.Length == 0 && ChoiceValue(ContextChoice) == "8192" && ChoiceValue(OutputChoice) == "262144" &&
            _gateway.Saves == writes, "explicit discard clears the key draft and loads saved limits without writing configuration");
        await SelectProviderAsync("Small / 两个模型");
    }

    private async Task BringOutputIntoViewAsync()
    {
        FrameworkElement target = OutputChoice;
        DependencyObject? parent = VisualTreeHelper.GetParent(target);
        while (parent is FrameworkElement element)
        {
            if (element is Border)
            {
                target = element;
                break;
            }
            parent = VisualTreeHelper.GetParent(element);
        }
        target.StartBringIntoView();
        await Task.Delay(350);
    }

    private async Task VerifyValidationFocusAsync()
    {
        var name = Element<TextBox>("NameBox");
        var id = Element<TextBox>("ProviderIdBox");
        var address = Element<TextBox>("BaseUrlBox");
        var key = Element<PasswordBox>("ApiKeyBox");
        var advanced = Element<Expander>("AdvancedSettings");
        string originalName = name.Text, originalId = id.Text, originalAddress = address.Text;
        int requests = _gateway.Requests.Count;
        NativeUi.SetText(name, "");
        NativeUi.Invoke(Button("SaveButton"));
        await SettleAsync();
        Check(name.FocusState != FocusState.Unfocused && _gateway.Requests.Count == requests,
            "missing connection names focus the original field without sending or resetting the form");
        NativeUi.SetText(name, originalName);
        NativeUi.SetText(id, "INVALID ID");
        advanced.IsExpanded = false;
        NativeUi.Invoke(Button("SaveButton"));
        await SettleAsync();
        Check(advanced.IsExpanded && id.FocusState != FocusState.Unfocused && id.Text == "INVALID ID" &&
            ChoiceValue(OutputChoice) == "262144" && _gateway.Requests.Count == requests,
            "an invalid hidden connection ID opens advanced settings and keeps its original draft and budget");
        NativeUi.SetText(id, originalId);
        NativeUi.SetText(address, "not-a-service-address");
        NativeUi.Invoke(Button("SaveButton"));
        await SettleAsync();
        Check(address.FocusState != FocusState.Unfocused && address.Text == "not-a-service-address" &&
            _gateway.Requests.Count == requests, "an invalid address receives focus before any gateway request");
        NativeUi.SetText(address, originalAddress);
        NativeUi.Invoke(Button("SaveButton"));
        await SettleAsync();
        Check(key.FocusState != FocusState.Unfocused && key.Password.Length == 0 && _gateway.Requests.Count == requests,
            "a missing key receives focus without adding a synthetic key or contacting an upstream service");
        advanced.IsExpanded = false;
        Check(Element<TextBlock>("DraftStateText").Text == UiText.Get("尚未保存"),
            "a restored new form shows not-saved configuration state rather than a verified-connection claim");
    }

    private async Task VerifyLongNamesAndShortWindowAsync()
    {
        string longName = "UI fixture / " + new string('名', 60);
        string longModelId = "fixture-long-model-" + new string('x', 140);
        NativeUi.SetText(Element<TextBox>("NameBox"), longName);
        await SetModelIdsAsync("fixture-model\nfixture-model-two\n" + longModelId);
        await SettleAsync();
        Check(Element<TextBlock>("DraftStateText").Text == UiText.Get("未保存的修改"),
            "editing a saved connection exposes its dirty state without requesting a save");
        await SaveAsync(262144);
        Check(Element<TextBlock>("DraftStateText").Text == UiText.Get("已保存") &&
            _gateway.Requests.Last().Connection.DisplayName == longName &&
            _gateway.Requests.Last().Connection.Models.Contains(longModelId),
            "saving a long name and model ID retains original payload values and resets only the draft presentation");
        var provider = Element<StackPanel>("ProviderList").Children.OfType<Button>().Single(button =>
            ToolTipService.GetToolTip(button)?.ToString() == longName);
        var providerLabel = ((StackPanel)provider.Content).Children.OfType<TextBlock>().First();
        Check(providerLabel.TextTrimming == TextTrimming.CharacterEllipsis && providerLabel.Text == longName,
            "saved-connection rows keep long names as data and provide a full tooltip for trimmed text");
        var option = Element<StackPanel>("ModelOptions").Children.OfType<CheckBox>().Single(item =>
            item.Content is StackPanel content && content.Children.OfType<TextBlock>().First().Text == longModelId);
        var modelLabel = ((StackPanel)option.Content).Children.OfType<TextBlock>().First();
        Check(modelLabel.TextWrapping == TextWrapping.NoWrap && modelLabel.TextTrimming == TextTrimming.CharacterEllipsis &&
            ToolTipService.GetToolTip(modelLabel)?.ToString() == longModelId,
            "long model IDs remain complete in accessible tooltips while their rows stay compact");
        ResizeInDips(480, 440);
        UiText.Initialize("en");
        await SettleAsync();
        var compact = Element<ComboBox>("CompactConnectionPicker");
        Check(NativeUi.IsVisible(compact) && ToolTipService.GetToolTip(compact)?.ToString() == longName &&
            compact.SelectedItem is ModelProvider selected && selected.DisplayName == longName,
            "minimum-size English layout keeps the full selected connection identity available");
        foreach (string buttonName in new[] { "NewConnectionButton", "ProbeButton", "SaveButton" })
        {
            var action = Button(buttonName);
            var bounds = action.TransformToVisual(_root).TransformBounds(new Rect(0, 0, action.ActualWidth, action.ActualHeight));
            Check(bounds.Left >= -1 && bounds.Top >= -1 && bounds.Right <= _root.ActualWidth + 1 &&
                bounds.Bottom <= _root.ActualHeight + 1 && NativeUi.IsVisible(action),
                $"minimum-size English layout keeps {buttonName} visible inside the native window");
        }
        Check(Element<ScrollViewer>("EditorScroll").ActualHeight >= 40 &&
            Element<TextBlock>("DraftStateText").Text == "Saved" && Element<TextBox>("NameBox").Text == longName,
            "short-window scrolling and localized saved-state feedback preserve the long connection draft");
        await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "minimum-size-long-connection-en.png"));
        UiText.Initialize("zh-CN");
        ResizeInDips(1120, 860);
        await SettleAsync();
    }

    private async Task ProbeAsync(int outputTokens)
    {
        int probes = _gateway.Probes;
        NativeUi.Invoke(Button("ProbeButton"));
        await WaitAsync(() => _gateway.Probes == probes + 1 && Button("ProbeButton").IsEnabled,
            $"explicit native probe round-trips output {outputTokens}");
        var request = _gateway.Requests.Last();
        Check(request.Path == "/api/models/test" && request.Connection.MaxOutputTokens == outputTokens,
            $"probe payload contains output {outputTokens}");
    }

    private async Task SaveAsync(int outputTokens)
    {
        int saves = _gateway.Saves;
        NativeUi.Invoke(Button("SaveButton"));
        await WaitAsync(() => _gateway.Saves == saves + 1 && Button("SaveButton").IsEnabled,
            $"explicit native save round-trips output {outputTokens}");
        Check(_gateway.Requests.Last() is { Path: "/api/models" } request && request.Connection.MaxOutputTokens == outputTokens,
            $"save payload contains output {outputTokens}");
    }

    private async Task RunAsync()
    {
        try
        {
            if (_closeDuringInitialization)
            {
                await WaitAsync(() => _root.XamlRoot is not null && _gateway.ListReads == 1 && !Button("SaveButton").IsEnabled,
                    "initializer fixture has an in-flight synthetic model catalog read");
                var initializerId = Element<TextBox>("ProviderIdBox");
                var initializerStatus = Element<TextBlock>("StatusText");
                var initializerProviders = Element<StackPanel>("ProviderList");
                string closingId = initializerId.Text;
                string initializerClosingStatus = initializerStatus.Text;
                _window!.Close();
                await WaitAsync(() => _windowClosed, "native model window closes promptly during initialization");
                await Task.Delay(900);
                Check(initializerId.Text == closingId && initializerStatus.Text == initializerClosingStatus &&
                    initializerProviders.Children.Count == 0 && _unhandled is null,
                    "late initialization does not populate or update a closed model window");
                Check(_gateway.Failure is null && _gateway.Saves == 0 && _gateway.Probes == 0,
                    "closing initialization leaves synthetic configuration unchanged and releases the cancelled read");
                File.AppendAllText(ResultPath, string.Join("\n", _checks) + $"\nPASS: {_checks.Count} native model initializer UI checks.");
                return;
            }
            await WaitAsync(() => _root.XamlRoot is not null && _gateway.ListReads == 1 && Button("SaveButton").IsEnabled &&
                Element<StackPanel>("ProviderList").Children.Count == 2, "production model window loads only the isolated fake connection list");
            Check(_window!.Title == "KYNXA 模型管理" && _window.ExtendsContentIntoTitleBar &&
                _window.AppWindow.Presenter is OverlappedPresenter { IsMinimizable: true, IsMaximizable: true, IsResizable: true },
                "native model window retains caption, minimize, maximize and resize");
            Check(ChoiceValue(OutputChoice) == "262144" && ChoiceValue(ContextChoice) == "1048576" && CustomOutput.Visibility == Visibility.Collapsed &&
                Element<TextBox>("CustomContextWindowBox").Visibility == Visibility.Collapsed,
                "new DeepSeek connections use the verified one-million context and independent 256K output ceiling");
            Check(!Element<Expander>("AdvancedSettings").IsExpanded && _gateway.Saves == 0 && _gateway.Probes == 0,
                "new output setting does not open advanced fields or save and test automatically");
            await BringOutputIntoViewAsync();
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "default-output-zh.png"));
            await VerifyValidationFocusAsync();

            SelectTokenChoice(ContextChoice, "8192");
            SelectTokenChoice(ContextChoice, "1048576");
            NativeUi.Invoke(Button("NewConnectionButton"));
            await ResolveDiscardAsync(discard: false);
            Check(ChoiceValue(ContextChoice) == "1048576" && _gateway.Saves == 0,
                "changing automatic context to a manual equivalent value remains a protected unsaved choice");
            await SelectProviderAsync("Legacy / 旧连接", expectDiscard: true);
            Check(ChoiceValue(OutputChoice) == "262144" && ChoiceValue(ContextChoice) == "8192" && _gateway.Saves == 0,
                "legacy connections missing both fields display independent defaults without rewriting them");
            NativeUi.SetText(Element<TextBox>("BaseUrlBox"), "https://api.deepseek.com");
            NativeUi.SetText(Element<TextBox>("ModelsBox"), "deepseek-flash");
            await SettleAsync();
            Check(ChoiceValue(ContextChoice) == "8192" && _gateway.Saves == 0,
                "editing an existing compatible 8K connection does not silently upgrade its context or save it");
            await SelectProviderAsync("Small / 两个模型", expectDiscard: true);
            Check(ChoiceValue(ContextChoice) == "1000000" && ChoiceValue(OutputChoice) == "custom" && CustomOutput.Text == "2048" &&
                CustomOutput.Visibility == Visibility.Visible, "existing explicit 2K output remains custom and preserves the saved one-million context");
            Check(Element<TextBox>("ProviderIdBox").IsReadOnly && Element<TextBox>("ModelsBox").Text.Contains("fixture-model-two") &&
                Element<PasswordBox>("ApiKeyBox").Password.Length == 0 && Element<TextBlock>("KeyHint").Text.Contains("已保存密钥"),
                "existing fixed connection identity, multiple models and blank-key preservation remain available");
            await SaveAsync(2048);
            await VerifyDiscardProtectionAsync();

            SelectTokenChoice(OutputChoice, "8192");
            Check(ChoiceValue(ContextChoice) == "1000000" && CustomOutput.Visibility == Visibility.Collapsed,
                "choosing 8K output leaves the one-million context unchanged");
            _gateway.ProbeDelayMs = 300;
            int delayedProbes = _gateway.Probes;
            NativeUi.Invoke(Button("ProbeButton"));
            await WaitAsync(() => _gateway.Probes == delayedProbes + 1 && !Button("ProbeButton").IsEnabled,
                "the real HTTP model probe enters its busy state");
            Check(!OutputChoice.IsEnabled && !CustomOutput.IsEnabled && !ContextChoice.IsEnabled && !Button("SaveButton").IsEnabled &&
                !Button("NewConnectionButton").IsEnabled, "busy model probes disable both token controls and connection actions");
            var probe = _gateway.Requests.Last().Connection;
            Check(probe.MaxOutputTokens == 8192 && probe.ContextWindowTokens == 1_000_000 && probe.Models.Length == 2 &&
                probe.ApiKey is null && probe.Protocol == "openai-responses", "probe sends output, context, two models and the existing protocol while keeping the saved key private");
            await WaitAsync(() => Button("ProbeButton").IsEnabled && Element<TextBlock>("StatusText").Text.Contains("已获取"),
                "completed model probes restore settings and expose discovered models");
            _gateway.ProbeDelayMs = 0;

            SelectTokenChoice(OutputChoice, "32768");
            await ProbeAsync(32768);
            await SaveAsync(32768);
            Check(ChoiceValue(OutputChoice) == "32768" && ChoiceValue(ContextChoice) == "1000000" &&
                !Element<Expander>("AdvancedSettings").IsExpanded, "save reloads 32K output without changing context or opening technical fields");
            SelectTokenChoice(OutputChoice, "custom");
            NativeUi.SetText(CustomOutput, "12345");
            await SettleAsync();
            await ProbeAsync(12345);
            await SaveAsync(12345);
            Check(ChoiceValue(OutputChoice) == "custom" && CustomOutput.Text == "12345" && ChoiceValue(ContextChoice) == "1000000",
                "non-preset output values survive save and load independently");

            int requestsBeforeInvalid = _gateway.Requests.Count;
            foreach (string invalid in new[] { "", "0", "1023", "262145", "1.5", "-1", "invalid" })
            {
                NativeUi.SetText(CustomOutput, invalid);
                await SettleAsync();
                NativeUi.Invoke(Button("SaveButton"));
                await SettleAsync();
                Check(_gateway.Requests.Count == requestsBeforeInvalid && Element<TextBlock>("StatusText").Text.Contains("1024–262144"),
                    $"invalid custom output '{invalid}' is rejected before save reaches the gateway");
                Check(CustomOutput.FocusState != FocusState.Unfocused,
                    $"invalid custom output '{invalid}' returns focus to its original visible input");
                NativeUi.Invoke(Button("ProbeButton"));
                await SettleAsync();
                Check(_gateway.Requests.Count == requestsBeforeInvalid,
                    $"invalid custom output '{invalid}' is rejected before probe reaches the gateway");
            }
            SelectTokenChoice(OutputChoice, "262144");
            UiText.Initialize("en");
            await SettleAsync();
            Check(_window.Title == "KYNXA Model Management" && NativeUi.Descendants<TextBlock>(_root).Any(text => text.Text == "Maximum output (tokens)"),
                "the output label switches to English in the open native window");
            Check(NativeUi.Descendants<TextBlock>(OutputChoice).Any(text => text.Text == "256K · Default") &&
                ChoiceValue(OutputChoice) == "262144" && ChoiceValue(ContextChoice) == "1000000",
                "the selected default caption localizes without changing either token limit");
            SelectTokenChoice(OutputChoice, "custom");
            NativeUi.SetText(CustomOutput, "12345");
            await SettleAsync();
            UiText.Initialize("zh-CN");
            await SettleAsync();
            Check(CustomOutput.Text == "12345" && ChoiceValue(ContextChoice) == "1000000" &&
                Element<TextBox>("NameBox").Text == "Small / 两个模型", "language switching preserves output drafts, one-million context and user-written connection names");
            Check(OutputChoice.FontSize == 13 && CustomOutput.FontSize == 13,
                "output controls retain the existing 13-point fonts");
            // Inspect the rendered focus borders rather than an obsolete window-local gray resource.
            // 检查实际渲染的焦点边框，不再读取已删除的窗口局部灰色资源。
            await BringOutputIntoViewAsync();
            Check(CustomOutput.Focus(FocusState.Keyboard), "the custom output editor accepts keyboard focus");
            await SettleAsync();
            var textFocusBorder = NativeUi.ByName<Border>(CustomOutput, "BorderElement");
            var accent = AppearanceService.ParseColor(AppearanceService.Current.Accent);
            Check(CustomOutput.FocusState != FocusState.Unfocused &&
                textFocusBorder.BorderBrush is SolidColorBrush textFocusBrush && textFocusBrush.Color == accent,
                "the actual focused custom output TextBox renders the shared palette accent border");
            Check(OutputChoice.Focus(FocusState.Keyboard), "the output selector accepts keyboard focus");
            await SettleAsync();
            var choiceFocusBorder = NativeUi.ByName<Border>(OutputChoice, "HighlightBackground");
            Check(OutputChoice.FocusState != FocusState.Unfocused && choiceFocusBorder.Opacity > 0 &&
                choiceFocusBorder.BorderBrush is SolidColorBrush choiceFocusBrush && choiceFocusBrush.Color == accent &&
                CustomOutput.Text == "12345" && ChoiceValue(OutputChoice) == "custom",
                "the actual focused output ComboBox renders the shared palette accent border without changing the token draft");
            await ProbeAsync(12345);
            ResizeInDips(760, 720);
            await SettleAsync();
            await BringOutputIntoViewAsync();
            var scroll = Element<ScrollViewer>("EditorScroll");
            var outputBounds = OutputChoice.TransformToVisual(scroll).TransformBounds(new Rect(0, 0, OutputChoice.ActualWidth, OutputChoice.ActualHeight));
            var customBounds = CustomOutput.TransformToVisual(scroll).TransformBounds(new Rect(0, 0, CustomOutput.ActualWidth, CustomOutput.ActualHeight));
            Check(OutputChoice.ActualWidth >= 100 && outputBounds.Left >= 0 && customBounds.Right <= scroll.ActualWidth + 1,
                "a narrow native window fits the output selector and custom value without horizontal clipping");
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "custom-output-narrow-zh.png"));
            ResizeInDips(1120, 860);
            await SettleAsync();
            SelectTokenChoice(OutputChoice, "65536");
            await BringOutputIntoViewAsync();
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "output-64k-zh.png"));

            await SelectPresetAsync("anthropic", expectDiscard: true);
            Check(ChoiceValue(OutputChoice) == "262144" && ChoiceValue(ContextChoice) == "200000" &&
                ChoiceValue(Element<ComboBox>("ProtocolBox")) == "anthropic-messages", "a new mixed Claude connection uses its smallest selected model window and retains its protocol");
            var modelOptions = Element<StackPanel>("ModelOptions").Children.OfType<CheckBox>().ToArray();
            foreach (var option in modelOptions)
                if (option.Content is StackPanel row && row.Children.OfType<TextBlock>().First().Text != "Claude Opus 5.5")
                    option.IsChecked = false;
            await SettleAsync();
            Check(ChoiceValue(ContextChoice) == "1000000" && Element<TextBox>("ModelsBox").Text == "claude-opus-5-5",
                "model checkboxes expand a new Claude draft's window while its manual editor stays collapsed");
            await SelectPresetAsync("kimi", expectDiscard: true);
            Check(ChoiceValue(ContextChoice) == "262144", "a new mixed Kimi draft uses the 256K models' verified window");
            UiText.Initialize("en");
            await SettleAsync();
            await SetModelIdsAsync("kimi-k3");
            Check(ChoiceValue(ContextChoice) == "1048576", "language refresh preserves automatic context selection for a new Kimi K3 draft");
            SelectTokenChoice(ContextChoice, "32768");
            await SetModelIdsAsync("kimi-k2.6");
            Check(ChoiceValue(ContextChoice) == "32768", "a manual context choice survives subsequent new-draft model changes");
            UiText.Initialize("zh-CN");
            await SettleAsync();
            await SelectPresetAsync("openai", expectDiscard: true);
            Check(ChoiceValue(ContextChoice) == "128000", "a mixed OpenAI preset does not claim one-million support for all models");
            await SetModelIdsAsync("gpt-6-astra");
            Check(ChoiceValue(ContextChoice) == "1050000", "a verified OpenAI model selection uses its real total context window");
            NativeUi.SetText(Element<TextBox>("BaseUrlBox"), "https://proxy.example.invalid/v1");
            await SettleAsync();
            Check(ChoiceValue(ContextChoice) == "8192", "an unverified proxy does not inherit an official model capability");
            await SelectPresetAsync("local-api", expectDiscard: true);
            Check(ChoiceValue(ContextChoice) == "8192" && ChoiceValue(OutputChoice) == "262144",
                "new local connections retain compatibility context defaults and independent output configuration");
            NativeUi.SetText(Element<TextBox>("NameBox"), "Local UI fixture / 临时");
            NativeUi.SetText(Element<TextBox>("ModelsBox"), "fixture-model\nfixture-model-two");
            await SettleAsync();
            SelectTokenChoice(ContextChoice, "32768");
            await ProbeAsync(262144);
            await SaveAsync(262144);
            var saved = _gateway.Requests.Last().Connection;
            Check(saved.ContextWindowTokens == 32768 && saved.MaxOutputTokens == 262144 && saved.Models.Length == 2 && saved.ApiKey is null,
                "a new local connection saves one shared output limit for multiple model IDs without a key");
            await VerifyLongNamesAndShortWindowAsync();

            _gateway.ProbeDelayMs = 1500;
            int probesBeforeClose = _gateway.Probes;
            NativeUi.Invoke(Button("ProbeButton"));
            await WaitAsync(() => _gateway.Probes == probesBeforeClose + 1 && !OutputChoice.IsEnabled,
                "closing fixture has an in-flight HTTP model probe");
            string closingStatus = Element<TextBlock>("StatusText").Text;
            _window.Close();
            await WaitAsync(() => _windowClosed, "native model window closes promptly during a busy probe");
            await Task.Delay(1700);
            Check(Element<TextBlock>("StatusText").Text == closingStatus && _unhandled is null,
                "a late model probe cannot update the closed window or raise an unhandled UI error");
            Check(_gateway.Failure is null && _unhandled is null, "the isolated gateway and native model window complete without unhandled failures");
            File.AppendAllText(ResultPath, string.Join("\n", _checks) + $"\nPASS: {_checks.Count} native model UI checks.\nPreviews: {_directory}");
        }
        catch (Exception error)
        {
            Environment.ExitCode = 1;
            File.AppendAllText(ResultPath, "FAIL after " + _checks.Count + " checks: " + error);
        }
        finally
        {
            if (!_windowClosed) _window?.Close();
            await _gateway.DisposeAsync();
            _anchor?.Close();
        }
    }
}
