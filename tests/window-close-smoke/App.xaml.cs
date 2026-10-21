using System.Diagnostics;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.Views;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace WindowCloseSmoke;

public partial class App : Application
{
    private sealed class FixtureWindow(Window window, ShellPage? page = null)
    {
        public Window Window { get; } = window;
        public ShellPage? Page { get; } = page;
        public bool IsClosed { get; set; }
    }

    private readonly string _directory = Path.Combine(Path.GetTempPath(), "kynxa-window-close-smoke-" + Guid.NewGuid().ToString("N"));
    private readonly List<string> _checks = [];
    private readonly List<FixtureWindow> _windows = [];
    private Window? _anchor;
    private Exception? _unhandled;
    private string ResultPath => Path.Combine(_directory, "result.txt");

    public App()
    {
        Directory.CreateDirectory(_directory);
        Environment.SetEnvironmentVariable("KYNXA_DATA_HOME", Path.Combine(_directory, "Data"));
        File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-window-close-smoke-latest.txt"), ResultPath);
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
        File.WriteAllText(ResultPath, "RUNNING: linked production WM_CLOSE, synthetic catalog saves, isolated windows\n");
        // Keep this test alive after the owned main window closes so the close result remains observable.
        // 主窗口关闭后仍保留隔离测试宿主，以便继续验证退出结果。
        _anchor = new Window { Content = new Grid() };
        _anchor.AppWindow.Hide();
        _ = RunAsync();
    }

    private void Check(bool condition, string description)
    {
        if (!condition) throw new InvalidOperationException(description);
        _checks.Add("PASS " + description);
    }

    private async Task WaitAsync(Func<bool> condition, string description, TimeSpan? timeout = null)
    {
        var elapsed = Stopwatch.StartNew();
        while (!condition())
        {
            if (_unhandled is not null) throw new InvalidOperationException("Unhandled window-close fixture error", _unhandled);
            if (elapsed.Elapsed > (timeout ?? TimeSpan.FromSeconds(5))) throw new TimeoutException(description);
            await Task.Delay(20);
        }
        Check(true, description);
    }

    private FixtureWindow TrackWindow(Window window, ShellPage? page = null)
    {
        var fixture = new FixtureWindow(window, page);
        window.Closed += (_, _) => fixture.IsClosed = true;
        _windows.Add(fixture);
        return fixture;
    }

    private async Task<FixtureWindow> OpenShellAsync(FakeProjectStore store)
    {
        var page = new ShellPage(store);
        var window = new Window { Title = "KYNXA window-close smoke", Content = page };
        window.AppWindow.Resize(new Windows.Graphics.SizeInt32(700, 450));
        KYNXA_Desktop.App.Window = window;
        page.AttachOwner(window);
        var fixture = TrackWindow(window, page);
        window.Activate();
        await WaitAsync(() => page.XamlRoot is not null && window.AppWindow.IsVisible,
            "an isolated native shell window becomes visible");
        return fixture;
    }

    private async Task<FixtureWindow> OpenSecondaryAsync(FixtureWindow owner)
    {
        var window = new Window { Title = "KYNXA synthetic secondary window", Content = new Grid() };
        window.AppWindow.Resize(new Windows.Graphics.SizeInt32(300, 200));
        var fixture = TrackWindow(window);
        owner.Page!.AttachSecondary(window);
        owner.Window.Closed += (_, _) => { if (!fixture.IsClosed) window.Close(); };
        window.Activate();
        await WaitAsync(() => window.AppWindow.IsVisible, "the synthetic secondary window becomes visible");
        return fixture;
    }

    private async Task CheckSlowSaveAsync()
    {
        var store = new FakeProjectStore();
        var shell = await OpenShellAsync(store);
        var page = shell.Page!;
        var secondary = await OpenSecondaryAsync(shell);
        page.WorkspaceTerminal.CloseCompletion = new(TaskCreationOptions.RunContinuationsAsynchronously);
        page.DocumentPanel.CloseCompletion = new(TaskCreationOptions.RunContinuationsAsynchronously);
        page.SetMetadataBusy(true);
        string draft = page.DraftEditor.Text;
        var elapsed = Stopwatch.StartNew();
        NativeUi.RequestClose(shell.Window);
        await WaitAsync(() => !shell.Window.AppWindow.IsVisible && !secondary.Window.AppWindow.IsVisible,
            "WM_CLOSE immediately hides the main and secondary windows while metadata is busy");
        Check(elapsed.Elapsed < TimeSpan.FromSeconds(1) && !shell.IsClosed && !page.IsEnabled && page.PauseCalls == 1,
            "visible exit feedback arrives within one second without ending the pending transaction");
        Check(store.SaveCalls == 0 && _anchor is { AppWindow.IsVisible: false },
            "close waits for existing metadata and preserves the hidden fixture host");
        NativeUi.RequestClose(shell.Window);
        await Task.Delay(100);
        Check(store.SaveCalls == 0 && page.PauseCalls == 1, "a repeated WM_CLOSE does not start another close operation");
        page.SetMetadataBusy(false);
        await WaitAsync(() => store.SaveCalls == 1, "finishing existing metadata starts one combined catalog save");
        Check(!shell.IsClosed && !shell.Window.AppWindow.IsVisible && store.SaveToken.CanBeCanceled,
            "the native window stays hidden and open until the cancellable save settles");
        Check(store.SavedProjects.SequenceEqual(["synthetic-project"]) && store.SavedChats.SequenceEqual(["synthetic-chat"]) &&
            page.ProjectCaptures == 1 && page.StandaloneCaptures == 1 && page.CapturedDraft == draft,
            "the single save includes both scopes and the captured synthetic draft");
        NativeUi.RequestClose(shell.Window);
        await Task.Delay(100);
        Check(store.SaveCalls == 1, "closing again during the pending PUT does not dispatch a duplicate save");
        Check(page.WorkspaceTerminal.CloseCalls == 0 && page.DocumentPanel.CloseCalls == 0,
            "owned terminal and browser cleanup waits for the formal save to succeed");
        store.CompleteSave();
        await WaitAsync(() => page.WorkspaceTerminal.CloseCalls == 1 && page.DocumentPanel.CloseCalls == 1,
            "successful persistence starts both owned cleanup operations without waiting for either one first");
        Check(!shell.IsClosed && !secondary.IsClosed, "owned windows remain alive while both cleanup operations are pending");
        page.WorkspaceTerminal.CloseCompletion.SetResult();
        await Task.Delay(80);
        Check(!shell.IsClosed, "completion of one cleanup operation does not bypass the remaining owned cleanup");
        page.DocumentPanel.CloseCompletion.SetResult();
        await WaitAsync(() => shell.IsClosed && secondary.IsClosed, "successful persistence closes both owned native windows");
        Check(store.SaveCalls == 1 && !store.SaveToken.IsCancellationRequested && _unhandled is null,
            "successful native exit completes exactly one transaction without UI errors");
    }

    private async Task CheckFailedSaveAsync()
    {
        var store = new FakeProjectStore { SaveFailure = new IOException("synthetic catalog save failure / 模拟保存失败") };
        var shell = await OpenShellAsync(store);
        var page = shell.Page!;
        var secondary = await OpenSecondaryAsync(shell);
        string draft = page.DraftEditor.Text;
        NativeUi.RequestClose(shell.Window);
        await WaitAsync(() => NativeUi.OpenDialog(page) is not null && shell.Window.AppWindow.IsVisible && secondary.Window.AppWindow.IsVisible,
            "a failed save restores the main and secondary windows and presents the save error");
        var errorDialog = NativeUi.OpenDialog(page)!;
        Check(!shell.IsClosed && store.SaveCalls == 1 && page.DraftEditor.Text == draft &&
            page.WorkspaceTerminal.CloseCalls == 0 && page.DocumentPanel.CloseCalls == 0 &&
            errorDialog.Content?.ToString()?.Contains("synthetic catalog save failure", StringComparison.Ordinal) == true,
            "save failure preserves the draft and explains the synthetic I/O error");
        NativeUi.InvokeDialogButton(errorDialog, primary: false);
        await WaitAsync(() => !page.IsClosing && page.IsEnabled && NativeUi.OpenDialog(page) is null,
            "returning from the save error restores native interaction");
        Check(!page.IsChatClosing && page.DraftEditor.Text == draft && store.SaveCalls == 1,
            "return leaves interrupted replies stopped and does not automatically repeat the failed save");
        store.SaveFailure = null;
        NativeUi.RequestClose(shell.Window);
        await WaitAsync(() => store.SaveCalls == 2 && !shell.Window.AppWindow.IsVisible,
            "a new explicit WM_CLOSE can start a fresh save after recovery");
        store.CompleteSave();
        await WaitAsync(() => shell.IsClosed && secondary.IsClosed, "the recovered native shell exits after its second save succeeds");
    }

    private async Task CheckPendingDialogAsync()
    {
        var store = new FakeProjectStore();
        var shell = await OpenShellAsync(store);
        var page = shell.Page!;
        Task<ContentDialogResult> metadataDialog = page.ShowPendingMetadataDialogAsync();
        await WaitAsync(() => NativeUi.OpenDialog(page) is not null,
            "the pending metadata fixture opens a real native confirmation");
        NativeUi.RequestClose(shell.Window);
        await WaitAsync(() => metadataDialog.IsCompleted && store.SaveCalls == 1 && !shell.Window.AppWindow.IsVisible,
            "close dismisses an existing metadata confirmation before waiting for its action");
        Check(await metadataDialog == ContentDialogResult.None && !shell.IsClosed,
            "hiding the pending confirmation cancels its action without approving or ending persistence");
        store.CompleteSave();
        await WaitAsync(() => shell.IsClosed, "the shell exits after the cancelled prompt and single catalog save settle");
    }

    private async Task CheckMigrationGuardAsync()
    {
        var store = new FakeProjectStore();
        var shell = await OpenShellAsync(store);
        var page = shell.Page!;
        StoragePaths.IsMigrating = true;
        NativeUi.RequestClose(shell.Window);
        await Task.Delay(150);
        Check(!shell.IsClosed && shell.Window.AppWindow.IsVisible && page.IsEnabled && store.SaveCalls == 0 && page.PauseCalls == 0,
            "data migration preserves the visible native shell and prevents close-side saves");
        StoragePaths.IsMigrating = false;
        store.CompleteSave();
        NativeUi.RequestClose(shell.Window);
        await WaitAsync(() => shell.IsClosed && store.SaveCalls == 1, "the native shell closes normally after migration finishes");
    }

    private async Task CheckSaveDeadlineAsync()
    {
        var store = new FakeProjectStore();
        var shell = await OpenShellAsync(store);
        var page = shell.Page!;
        string draft = page.DraftEditor.Text;
        NativeUi.RequestClose(shell.Window);
        await WaitAsync(() => store.SaveCalls == 1 && !shell.Window.AppWindow.IsVisible,
            "a stalled save begins while the native shell is already hidden");
        await WaitAsync(() => store.SaveToken.IsCancellationRequested && shell.Window.AppWindow.IsVisible && NativeUi.OpenDialog(page) is not null,
            "the shared ten-second deadline cancels a stalled save and restores the native shell", TimeSpan.FromSeconds(13));
        Check(!shell.IsClosed && store.SaveCalls == 1 && page.DraftEditor.Text == draft,
            "exit timeout preserves the draft without a second write or an indefinite hidden window");
        NativeUi.InvokeDialogButton(NativeUi.OpenDialog(page)!, primary: false);
        await WaitAsync(() => !page.IsClosing && page.IsEnabled,
            "returning from the exit timeout restores interaction without automatic replay");
        shell.Window.Close();
    }

    private async Task CheckMigrationDuringCloseAsync()
    {
        var store = new FakeProjectStore();
        var shell = await OpenShellAsync(store);
        var page = shell.Page!;
        page.SetMetadataBusy(true);
        NativeUi.RequestClose(shell.Window);
        await WaitAsync(() => !shell.Window.AppWindow.IsVisible, "the migration-race fixture begins a pending native close");
        StoragePaths.IsMigrating = true;
        page.SetMetadataBusy(false);
        await WaitAsync(() => shell.Window.AppWindow.IsVisible && !page.IsClosing,
            "migration starting during close restores the shell before a catalog write");
        Check(!shell.IsClosed && !page.IsEnabled && store.SaveCalls == 0 && page.ProjectNotice.IsOpen && NativeUi.OpenDialog(page) is null,
            "the migration race has no force-close option and keeps migration interaction protected");
        StoragePaths.IsMigrating = false;
        store.CompleteSave();
        NativeUi.RequestClose(shell.Window);
        await WaitAsync(() => shell.IsClosed, "the restored shell can exit after the raced migration finishes");
    }

    private async Task RunAsync()
    {
        try
        {
            if (Environment.GetEnvironmentVariable("KYNXA_WINDOW_TRAY_ONLY") == "1")
            {
                await CheckTrayLifecycleAsync();
                await CheckTrayExitRecoveryAsync();
            }
            else
            {
                await CheckSlowSaveAsync();
                await CheckFailedSaveAsync();
                await CheckPendingDialogAsync();
                await CheckMigrationGuardAsync();
                await CheckSaveDeadlineAsync();
                await CheckMigrationDuringCloseAsync();
            }
            Check(_unhandled is null, "all linked production close flows finish without unhandled native UI errors");
            File.AppendAllText(ResultPath, string.Join("\n", _checks) + $"\nPASS: {_checks.Count} native window-close UI checks.");
        }
        catch (Exception error)
        {
            Environment.ExitCode = 1;
            File.AppendAllText(ResultPath, string.Join("\n", _checks) + "\nFAIL after " + _checks.Count + " checks: " + error);
        }
        finally
        {
            StoragePaths.IsMigrating = false;
            foreach (var fixture in _windows) fixture.Page?.Store.CompleteSave();
            foreach (var fixture in _windows.AsEnumerable().Reverse())
                if (!fixture.IsClosed) fixture.Window.Close();
            _anchor?.Close();
        }
    }
}
