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
            if (DateTime.UtcNow > deadline) throw new TimeoutException(description);
            await Task.Delay(25);
        }
        Check(true, description);
    }

    private static Task SettleAsync() => Task.Delay(180);

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

    private async Task SelectProviderAsync(string name)
    {
        var panel = Element<StackPanel>("ProviderList");
        var button = panel.Children.OfType<Button>().Single(item => item.Content is StackPanel content &&
            content.Children.OfType<TextBlock>().First().Text == name);
        NativeUi.Invoke(button);
        await SettleAsync();
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

            await SelectProviderAsync("Legacy / 旧连接");
            Check(ChoiceValue(OutputChoice) == "262144" && ChoiceValue(ContextChoice) == "8192" && _gateway.Saves == 0,
                "legacy connections missing both fields display independent defaults without rewriting them");
            NativeUi.SetText(Element<TextBox>("BaseUrlBox"), "https://api.deepseek.com");
            NativeUi.SetText(Element<TextBox>("ModelsBox"), "deepseek-flash");
            await SettleAsync();
            Check(ChoiceValue(ContextChoice) == "8192" && _gateway.Saves == 0,
                "editing an existing compatible 8K connection does not silently upgrade its context or save it");
            await SelectProviderAsync("Small / 两个模型");
            Check(ChoiceValue(ContextChoice) == "1000000" && ChoiceValue(OutputChoice) == "custom" && CustomOutput.Text == "2048" &&
                CustomOutput.Visibility == Visibility.Visible, "existing explicit 2K output remains custom and preserves the saved one-million context");
            Check(Element<TextBox>("ProviderIdBox").IsReadOnly && Element<TextBox>("ModelsBox").Text.Contains("fixture-model-two") &&
                Element<PasswordBox>("ApiKeyBox").Password.Length == 0 && Element<TextBlock>("KeyHint").Text.Contains("已保存密钥"),
                "existing fixed connection identity, multiple models and blank-key preservation remain available");
            await SaveAsync(2048);

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
            Check(OutputChoice.FontSize == 13 && CustomOutput.FontSize == 13 &&
                _root.Resources["TextControlBorderBrushFocused"] is SolidColorBrush { Color.R: 136, Color.G: 136, Color.B: 136 },
                "output controls share the existing 13-point fonts and neutral focus border");
            await ProbeAsync(12345);
            _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(760, 720));
            await SettleAsync();
            await BringOutputIntoViewAsync();
            var scroll = Element<ScrollViewer>("EditorScroll");
            var outputBounds = OutputChoice.TransformToVisual(scroll).TransformBounds(new Rect(0, 0, OutputChoice.ActualWidth, OutputChoice.ActualHeight));
            var customBounds = CustomOutput.TransformToVisual(scroll).TransformBounds(new Rect(0, 0, CustomOutput.ActualWidth, CustomOutput.ActualHeight));
            Check(OutputChoice.ActualWidth >= 100 && outputBounds.Left >= 0 && customBounds.Right <= scroll.ActualWidth + 1,
                "a narrow native window fits the output selector and custom value without horizontal clipping");
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "custom-output-narrow-zh.png"));
            _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(1120, 860));
            await SettleAsync();
            SelectTokenChoice(OutputChoice, "65536");
            await BringOutputIntoViewAsync();
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "output-64k-zh.png"));

            var presets = Element<ComboBox>("PresetBox");
            presets.SelectedItem = ModelPresets.All.Single(preset => preset.Id == "anthropic");
            await SettleAsync();
            Check(ChoiceValue(OutputChoice) == "262144" && ChoiceValue(ContextChoice) == "200000" &&
                ChoiceValue(Element<ComboBox>("ProtocolBox")) == "anthropic-messages", "a new mixed Claude connection uses its smallest selected model window and retains its protocol");
            var modelOptions = Element<StackPanel>("ModelOptions").Children.OfType<CheckBox>().ToArray();
            foreach (var option in modelOptions)
                if (option.Content is StackPanel row && row.Children.OfType<TextBlock>().First().Text != "Claude Opus 5.5")
                    option.IsChecked = false;
            await SettleAsync();
            Check(ChoiceValue(ContextChoice) == "1000000" && Element<TextBox>("ModelsBox").Text == "claude-opus-5-5",
                "model checkboxes expand a new Claude draft's window while its manual editor stays collapsed");
            presets.SelectedItem = ModelPresets.All.Single(preset => preset.Id == "kimi");
            await SettleAsync();
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
            presets.SelectedItem = ModelPresets.All.Single(preset => preset.Id == "openai");
            await SettleAsync();
            Check(ChoiceValue(ContextChoice) == "128000", "a mixed OpenAI preset does not claim one-million support for all models");
            await SetModelIdsAsync("gpt-6-astra");
            Check(ChoiceValue(ContextChoice) == "1050000", "a verified OpenAI model selection uses its real total context window");
            NativeUi.SetText(Element<TextBox>("BaseUrlBox"), "https://proxy.example.invalid/v1");
            await SettleAsync();
            Check(ChoiceValue(ContextChoice) == "8192", "an unverified proxy does not inherit an official model capability");
            presets.SelectedItem = ModelPresets.All.Single(preset => preset.Id == "local-api");
            await SettleAsync();
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
