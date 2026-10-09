using System.Diagnostics;
using System.Text.Json;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.Views;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Foundation;
using Windows.Graphics;

namespace RetrievalUiSmoke;

public partial class App : Application
{
    private readonly string _directory = Path.Combine(Path.GetTempPath(), "kynxa-retrieval-ui-smoke-" + Guid.NewGuid().ToString("N"));
    private readonly List<string> _checks = [];
    private readonly FakeRetrievalApi _api = new();
    private RetrievalSettingsWindow? _window;
    private Window? _anchor;
    private FrameworkElement _root = null!;
    private Exception? _unhandled;
    private string ResultPath => Path.Combine(_directory, "result.txt");

    public App()
    {
        Directory.CreateDirectory(_directory);
        File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-retrieval-ui-smoke-latest.txt"), ResultPath);
        InitializeComponent();
        UnhandledException += (_, args) => { _unhandled = args.Exception; args.Handled = true; };
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        _api.Sources = [new("synthetic_global", "Global source / 全局资料", "user", null, "synthetic-global.txt", "ready", Revision: 1),
            new("synthetic_project", "Project source / 工作资料", "project", _api.ProjectId, "synthetic-project.txt", "ready", Revision: 1)];
        _anchor = new Window { Content = new Grid() };
        _anchor.AppWindow.Hide();
        OpenWindow();
        _ = RunAsync();
    }

    private void OpenWindow(bool project = false)
    {
        _window = project ? new(_api.ProjectId, "Synthetic work / 中文", _directory, _api) : new(api: _api);
        _root = (FrameworkElement)_window.Content;
        _window.AppWindow.Move(new PointInt32(30, 30));
        _window.AppWindow.Show(activateWindow: false);
    }

    private ComboBox Picker(string id) => NativeUi.ById<ComboBox>(_root, id);
    private Button Button(string id) => NativeUi.ById<Button>(_root, id);
    private CheckBox Toggle(string id) => NativeUi.ById<CheckBox>(_root, id);
    private InfoBar Notice => NativeUi.ById<InfoBar>(_root, "RetrievalSettingsNotice");

    private async Task RunAsync()
    {
        bool passed = false;
        File.WriteAllText(ResultPath, "RUNNING: isolated native retrieval UI\n");
        try
        {
            await WaitAsync(() => _root.XamlRoot is not null && Picker("RetrievalWebDepth").SelectedIndex == 0 && !_window!.HasPendingChanges,
                "Global window loads without creating or updating settings.");
            Check(_api.GlobalWrites == 0 && _api.ProjectWrites == 0, "Opening settings does not perform writes.");
            Check(Picker("RetrievalWebProvider").Items.Count == 2, "Search provider list includes automatic selection and configured services.");
            Check(!Notice.IsOpen, "Normal loading shows no saved notification.");
            Check(!NativeUi.Descendants<Button>(_root).Any(button => button.Content?.ToString() == UiText.Get("关闭")), "Window uses native caption controls and has no bottom Close button.");
            Check(Picker("RetrievalRerankMode").SelectedIndex == 0, "Optional reranking stays disabled until selected.");
            Picker("RetrievalRerankMode").SelectedIndex = 1;
            await WaitAsync(() => _api.Global.Local.RerankProfileId == "builtin-multilingual-reranker" && !_window!.HasPendingChanges,
                "Enabling conditional reranking saves its stable profile reference.");
            Picker("RetrievalRerankMode").SelectedIndex = 0;
            await WaitAsync(() => _api.Global.Local.RerankProfileId is null && !_window!.HasPendingChanges,
                "Disabling reranking explicitly clears the saved profile.");
            await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "global-zh.png"));
            Picker("RetrievalWebDepth").SelectedIndex = 1;
            await WaitAsync(() => _api.Global.Web.Depth == "deep" && !_window!.HasPendingChanges, "Selecting search depth saves immediately.");
            Check(!Notice.IsOpen, "Successful auto-save does not add status clutter.");
            _api.ConflictNextSave = true;
            int beforeConflict = _api.GlobalWrites;
            Picker("RetrievalWebDepth").SelectedIndex = 0;
            await WaitAsync(() => Notice.IsOpen && !_window!.HasPendingChanges, "Version conflict refreshes the current settings and displays a useful error.");
            Check(_api.GlobalWrites == beforeConflict + 1 && Picker("RetrievalWebDepth").SelectedIndex == 1,
                "A stale selection is not silently replayed after conflict.");
            UiText.Initialize("en");
            await Task.Delay(100);
            Check(_window!.Title == "KYNXA · Retrieval and web search", "Open window title changes language without restart.");
            Check(((ComboBoxItem)Picker("RetrievalWebDepth").Items[0]).Content?.ToString() == "Standard", "Search choices are translated live.");
            Check(((ComboBoxItem)Picker("RetrievalRerankMode").Items[1]).Content?.ToString() == "Enable when needed",
                "Conditional reranking choices change language without restart.");
            Check(NativeUi.Descendants<TextBlock>(_root).Any(label => label.Text == "Ready locally"), "Dynamic model status is translated live.");
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "global-en.png"));
            foreach (int width in new[] { 540, 880 })
            {
                _window.AppWindow.Resize(new SizeInt32(width, 760));
                await Task.Delay(100);
                foreach (string id in new[] { "RetrievalRerankMode", "RetrievalWebMode", "RetrievalWebProvider", "RetrievalWebDepth", "RetrievalWebLanguage", "RetrievalRebuildIndex" })
                {
                    var control = NativeUi.ById<FrameworkElement>(_root, id);
                    var bounds = control.TransformToVisual(_root).TransformBounds(new Rect(0, 0, control.ActualWidth, control.ActualHeight));
                    Check(bounds.X >= -1 && bounds.Right <= _root.ActualWidth + 1, $"{id} remains within the viewport at width {width}.");
                }
            }
            bool prematurelyClosed = false;
            _window.Closed += (_, _) => prematurelyClosed = true;
            _api.SaveGate = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            Picker("RetrievalWebLanguage").SelectedIndex = 2;
            await WaitAsync(() => _window.HasPendingChanges, "Auto-save exposes its pending state during an outstanding request.");
            NativeUi.RequestClose(_window);
            await Task.Delay(100);
            Check(!prematurelyClosed && _window.HasPendingChanges, "Native Close waits for pending writes instead of losing their result.");
            _api.SaveGate.SetResult();
            await WaitAsync(() => !_window.HasPendingChanges && _api.Global.Web.Language == "en", "Completing the pending save restores the window's normal close behavior.");
            _api.SaveGate = null;
            _window.CloseForOwner();
            UiText.Initialize("zh-CN");
            OpenWindow(project: true);
            await WaitAsync(() => _root.XamlRoot is not null && !_window!.HasPendingChanges && Toggle("RetrievalInheritGlobal").IsChecked == true,
                "Project window defaults to inherited global settings.");
            Check(!Picker("RetrievalWebDepth").IsEnabled, "Inherited choices remain visible without misleading local editing.");
            Check(NativeUi.Descendants<Button>(_root).Any(button => Microsoft.UI.Xaml.Automation.AutomationProperties.GetAutomationId(button) == "RetrievalRemoveSource-synthetic_project"),
                "Project sources are manageable in their own settings window.");
            Check(!NativeUi.Descendants<Button>(_root).Any(button => Microsoft.UI.Xaml.Automation.AutomationProperties.GetAutomationId(button) == "RetrievalRemoveSource-synthetic_global"),
                "Inherited global sources cannot be accidentally removed from the project window.");
            Toggle("RetrievalMountedFolder").IsChecked = true;
            await WaitAsync(() => _api.Project.IndexingSources.MountedFolder.Enabled && !_window!.HasPendingChanges,
                "Mounted-folder indexing is an explicit per-project selection.");
            Check(_api.Project.Overrides.Local is null && _api.Project.Overrides.Web is null, "Folder indexing does not replace inherited web and local settings.");
            Toggle("RetrievalInheritGlobal").IsChecked = false;
            await WaitAsync(() => Picker("RetrievalWebDepth").IsEnabled && !_window!.HasPendingChanges, "A project can opt into its own settings.");
            Picker("RetrievalWebDepth").SelectedIndex = 0;
            await WaitAsync(() => _api.Project.Effective.Web.Depth == "standard" && !_window!.HasPendingChanges, "Project override changes only that project.");
            Check(_api.Global.Web.Depth == "deep", "Global settings are unchanged by project overrides.");
            Toggle("RetrievalInheritGlobal").IsChecked = true;
            await WaitAsync(() => _api.Project.Overrides.Web is null && Picker("RetrievalWebDepth").SelectedIndex == 1 && !_window!.HasPendingChanges,
                "Restoring inheritance removes the project override.");
            NativeUi.Invoke(Button("RetrievalRebuildIndex"));
            await WaitAsync(() => _api.Rebuilds == 1 && Button("RetrievalCancelIndex").Visibility == Visibility.Visible && !_window!.HasPendingChanges,
                "Rebuild starts a cancellable background job.");
            NativeUi.Invoke(Button("RetrievalCancelIndex"));
            await WaitAsync(() => _api.CancelledJobs == 1 && Button("RetrievalCancelIndex").Visibility == Visibility.Collapsed && !_window!.HasPendingChanges,
                "Cancelling the owned index job leaves the settings window usable.");
            await VerifyJobStatesAsync();
            await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "project-zh.png"));
            passed = true;
        }
        catch (Exception error) { Environment.ExitCode = 1; File.AppendAllText(ResultPath, "FAIL: " + error); }
        finally
        {
            File.WriteAllText(Path.Combine(_directory, "result.json"), JsonSerializer.Serialize(new { passed, checks = _checks,
                _api.GlobalWrites, _api.ProjectWrites, _api.Rebuilds, _api.CancelledJobs,
                inputMethod = "Real WinUI controls and automation peers; no physical mouse or keyboard claim" }, new JsonSerializerOptions { WriteIndented = true }));
            if (passed) File.AppendAllText(ResultPath, $"PASS: {_checks.Count} native retrieval UI checks\n");
            _window?.CloseForOwner();
            _anchor?.Close();
        }
    }

    private async Task WaitAsync(Func<bool> condition, string description)
    {
        var timer = Stopwatch.StartNew();
        while (!condition())
        {
            if (_unhandled is not null) throw new InvalidOperationException("Unexpected UI failure", _unhandled);
            if (timer.ElapsedMilliseconds > 7000) throw new TimeoutException(description);
            await Task.Delay(40);
        }
        await Task.Delay(80);
        Check(condition(), description);
    }

    private void Check(bool condition, string description)
    {
        if (!condition) throw new InvalidOperationException(description);
        _checks.Add(description);
        File.AppendAllText(ResultPath, "PASS: " + description + "\n");
    }
}
