using System.Diagnostics;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Foundation;
using Windows.Graphics;

namespace MemoryUiSmoke;

public partial class App : Application
{
    private readonly string _testDirectory = Path.Combine(Path.GetTempPath(), "kynxa-memory-ui-smoke-" + Guid.NewGuid().ToString("N"));
    private readonly List<string> _checks = [];
    private readonly List<string> _previews = [];
    private readonly MemoryTarget _user = new(MemoryScopes.User, null, "全局记忆");
    private readonly MemoryTarget _chat = new(MemoryScopes.Chat, Guid.NewGuid(), "Synthetic chat / 原样名称");
    private readonly MemoryTarget _emptyChat = new(MemoryScopes.Chat, Guid.NewGuid(), "Empty synthetic chat");
    private readonly MemoryTarget _project = new(MemoryScopes.Project, Guid.NewGuid(), "Synthetic work / 原样名称");
    private readonly MemoryTarget _archivedProject = new(MemoryScopes.Project, Guid.NewGuid(), "Archived synthetic work", IsArchived: true);
    private readonly FakeMemoryApi _api = new();
    private MemoryManagementWindow? _window;
    private Window? _anchor;
    private FrameworkElement _root = null!;
    private MemoryManagementViewModel _viewModel = null!;
    private Exception? _unhandledError;
    private bool _closed;
    private string ResultPath => Path.Combine(_testDirectory, "result.txt");
    private ListView EntryList => NativeUi.ById<ListView>(_root, "MemoryEntryList");
    private TextBox ContentBox => NativeUi.ById<TextBox>(_root, "MemoryContentBox");
    private TextBox SearchBox => NativeUi.ById<TextBox>(_root, "MemorySearchBox");
    private ComboBox ContextPicker => NativeUi.ById<ComboBox>(_root, "MemoryContextPicker");
    private InfoBar StatusBar => NativeUi.ById<InfoBar>(_root, "MemoryStatusBar");

    public App()
    {
        Directory.CreateDirectory(_testDirectory);
        string dataRoot = Path.Combine(_testDirectory, "Data");
        Directory.CreateDirectory(dataRoot);
        Environment.SetEnvironmentVariable("KYNXA_DATA_HOME", dataRoot);
        Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", "http://127.0.0.1:1");
        File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-memory-ui-smoke-latest.txt"), ResultPath);
        InitializeComponent();
        UnhandledException += (_, args) =>
        {
            _unhandledError = args.Exception;
            args.Handled = true;
            File.AppendAllText(ResultPath, "\nUNHANDLED: " + args.Exception);
        };
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        File.WriteAllText(ResultPath, "RUNNING: isolated native memory management UI checks\n");
        _api.Seed(_user,
            FakeMemoryApi.Entry(_user, "手动偏好：使用中文回答，保持简洁。", MemoryKinds.Preference),
            FakeMemoryApi.Entry(_user, "归档来源：保留已确认的上下文。", archived: true, fromMessage: true),
            FakeMemoryApi.Entry(_user, "不可用来源：已暂停使用。", active: false, available: false, fromMessage: true),
            FakeMemoryApi.Entry(_user, "决定：只有用户确认的记忆进入模型上下文。", MemoryKinds.Decision));
        _api.Seed(_chat, FakeMemoryApi.Entry(_chat, "CHAT_ONLY synthetic source", fromMessage: true));
        _api.Seed(_emptyChat);
        _api.Seed(_project, FakeMemoryApi.Entry(_project, "PROJECT_ONLY synthetic decision", MemoryKinds.Decision));
        _api.Seed(_archivedProject, FakeMemoryApi.Entry(_archivedProject, "ARCHIVED_PROJECT_ONLY synthetic memory"));
        _viewModel = new MemoryManagementViewModel(_api);
        // A hidden lifetime window permits checking a late response after the real memory window closes.
        _anchor = new Window { Content = new Grid() };
        _anchor.AppWindow.Hide();
        _window = new MemoryManagementWindow([_chat, _emptyChat, _project, _archivedProject], viewModel: _viewModel);
        _window.Closed += (_, _) => _closed = true;
        _root = (FrameworkElement)_window.Content;
        _window.Activate();
        _ = RunAsync();
    }

    private async Task RunAsync()
    {
        bool passed = false;
        try
        {
            await WaitAsync(() => _root.XamlRoot is not null && _viewModel.IsLoaded && !_viewModel.IsBusy,
                "native memory window loads the injected global snapshot");
            _window!.AppWindow.Resize(new SizeInt32(1120, 860));
            await SettleAsync();
            Check(_viewModel.Target?.Scope == MemoryScopes.User && EntryList.Items.Count == 4,
                "default global scope shows all confirmed entries, including archived and unavailable sources");
            Check(_viewModel.IsNew && ContentBox.Text.Length == 0 && !Button("SaveMemoryButton").IsEnabled,
                "initial blank editor cannot save an empty memory");
            Check(!Button("DeleteMemoryButton").IsEnabled && Button("NewMemoryButton").IsEnabled,
                "initial editor exposes New and disables Delete");
            await CaptureAsync("01-list-zh");
            await CheckEditingAndDialogsAsync();
            await CheckErrorsAndConflictsAsync();
            await CheckTargetsAsync();
            await CheckWindowLifecycleAsync();
            await CheckGlobalOnlyWindowAsync();
            passed = true;
        }
        catch (Exception error)
        {
            Environment.ExitCode = 1;
            File.AppendAllText(ResultPath, "\nFAIL after " + _checks.Count + " checks: " + error);
            if (_window is not null && !_closed)
            {
                try { await CaptureAsync("failure"); }
                catch (Exception captureError) { File.AppendAllText(ResultPath, "\nCapture failure: " + captureError.Message); }
            }
        }
        finally
        {
            File.WriteAllText(Path.Combine(_testDirectory, "result.json"), JsonSerializer.Serialize(new
            {
                passed,
                checks = _checks,
                previews = _previews,
                fakeRequests = _api.Requests,
                writes = _api.Writes,
                editorState = new { _viewModel.EditorContent, _viewModel.EditorKind, _viewModel.IsLoaded,
                    _viewModel.IsBusy, _viewModel.HasChanges, _viewModel.CanSave, _viewModel.Status,
                    nativeContent = ContentBox.Text,
                    nativeKind = NativeUi.ById<ComboBox>(_root, "MemoryKindPicker").SelectedItem?.ToString(),
                    saveEnabled = Button("SaveMemoryButton").IsEnabled },
                dataRoot = Environment.GetEnvironmentVariable("KYNXA_DATA_HOME"),
                inputMethod = "Real WinUI automation peers and programmatic ComboBox selection; no physical pointer or keyboard claim"
            }, new JsonSerializerOptions { WriteIndented = true }));
            if (passed) File.AppendAllText(ResultPath,
                $"\nPASS: {_checks.Count} native memory UI checks. Previews: {string.Join(", ", _previews.Select(Path.GetFileName))}\n");
            if (!_closed) _window?.CloseForOwner();
            _anchor?.Close();
        }
    }

    private Button Button(string id) => NativeUi.ById<Button>(_root, id);
    private TextBlock Label(string name) => NativeUi.ByName<TextBlock>(_root, name);
    private static MemoryEntry EntryFromItem(object item) => ((MemoryDisplayRow)item).Entry;

    private async Task OpenEntryAsync(int index)
    {
        var entry = EntryFromItem(EntryList.Items[index]);
        await NativeUi.InvokeListItemAsync(EntryList, EntryList.Items[index]);
        await WaitAsync(() => _viewModel.SelectedEntry?.Id == entry.Id && ContentBox.Text == entry.Content,
            "native memory row opens the editor for its stable entry ID");
    }

    private void Check(bool condition, string description)
    {
        if (!condition) throw new InvalidOperationException(description);
        _checks.Add(description);
        File.AppendAllText(ResultPath, "PASS: " + description + "\n");
    }

    private async Task WaitAsync(Func<bool> predicate, string description, int timeoutMs = 7000)
    {
        var elapsed = Stopwatch.StartNew();
        while (!predicate())
        {
            if (_unhandledError is not null) throw new InvalidOperationException("Unexpected UI event failure", _unhandledError);
            if (elapsed.ElapsedMilliseconds > timeoutMs) throw new TimeoutException(description);
            await Task.Delay(40);
        }
        await SettleAsync();
        Check(predicate(), description);
    }

    private async Task SettleAsync()
    {
        if (!_closed) _root.UpdateLayout();
        await Task.Delay(100);
    }

    private async Task<ContentDialog> WaitForDialogAsync(string description)
    {
        await WaitAsync(() => NativeUi.OpenDialog(_root) is not null, description);
        return NativeUi.OpenDialog(_root)!;
    }

    private async Task DismissDialogAsync(ContentDialog dialog, bool primary)
    {
        var closed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        void DialogClosed(ContentDialog _, ContentDialogClosedEventArgs __) => closed.TrySetResult();
        dialog.Closed += DialogClosed;
        try
        {
            NativeUi.InvokeDialogButton(dialog, primary);
            await closed.Task.WaitAsync(TimeSpan.FromSeconds(5));
            await WaitAsync(() => NativeUi.OpenDialog(_root) is null, "native dialog button closes the confirmation");
        }
        finally { dialog.Closed -= DialogClosed; }
    }

    private async Task SaveAsync(MemoryManagementStatus expected = MemoryManagementStatus.Saved)
    {
        NativeUi.Invoke(Button("SaveMemoryButton"));
        await WaitAsync(() => !_viewModel.IsBusy && _viewModel.Status == expected,
            "native Save reaches the backend and presents " + expected);
    }

    private async Task CaptureAsync(string name)
    {
        await SettleAsync();
        string path = Path.Combine(_testDirectory, name + ".png");
        await NativeWindowCapture.CaptureAsync(_window!, path);
        _previews.Add(path);
    }

    private void CheckControlBounds(int width)
    {
        foreach (string id in new[] { "MemoryScopeChatButton", "MemoryScopeProjectButton", "MemoryScopeUserButton",
            "MemorySearchBox", "MemoryEntryList", "NewMemoryButton", "RefreshMemoryButton", "MemoryKindPicker",
            "MemoryContentBox", "SaveMemoryButton" })
        {
            var control = NativeUi.ById<FrameworkElement>(_root, id);
            var bounds = control.TransformToVisual(_root).TransformBounds(new Rect(0, 0, control.ActualWidth, control.ActualHeight));
            Check(bounds.X >= -1 && bounds.Right <= _root.ActualWidth + 1,
                id + " stays within the native viewport at width " + width);
            if (id == "SaveMemoryButton") Check(bounds.Y >= 0 && bounds.Bottom <= _root.ActualHeight + 1,
                "Save remains visible within the native viewport at width " + width);
        }
    }
}
