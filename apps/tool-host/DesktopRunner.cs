using System.Diagnostics;
using static KYNXA.ToolHost.DesktopNativeMethods;

namespace KYNXA.ToolHost;

internal static class DesktopRunner
{
    private static readonly string[] Operations = ["apps", "windows", "screenshot", "read", "launch", "window", "activate", "move", "click", "scroll", "drag", "type", "key"];

    internal static object Capabilities() => new
    {
        protocolVersion = 1, boundary = "host-desktop", available = DesktopTarget.IsInteractive(),
        interactiveWindows = DesktopTarget.IsInteractive(), operations = Operations, keys = DesktopInput.SupportedKeys,
        imageMaxBytes = 4 * 1024 * 1024, coordinates = "client-physical-pixels", canElevate = false,
        features = new { backgroundLaunch = "best-effort-no-activate", screenshotCrop = true, passwordLocators = true,
            boundedRead = true, readTimeoutDefaultMs = 3000, readTimeoutMaxMs = 5000,
            windowModes = new[] { "resize", "maximize", "minimize", "restore" } },
        limitations = new[] { "Native host-desktop access, outside AppContainer. Each action requires the broker's permission.",
            "Input requires an explicit matching window/process and the visible foreground client area.",
            "Reading uses visible UI Automation, not a full browser DOM. No protected-desktop or UIPI bypass." }
    };

    internal static async Task<Dictionary<string, object?>> RunAsync(DesktopRequest request, CancellationToken cancellation)
    {
        if (request.Action == "read" && request.TimeoutMs is < 500 or > 5000)
            throw new DesktopException("DESKTOP_INVALID_REQUEST", "The read timeout must be between 500 and 5000 milliseconds.");
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        int timeoutMs = request.Action == "read" ? request.TimeoutMs : 18000;
        deadline.CancelAfter(timeoutMs);
        var completion = new TaskCompletionSource<Dictionary<string, object?>>(TaskCreationOptions.RunContinuationsAsynchronously);
        var worker = new Thread(() =>
        {
            try { completion.TrySetResult(Run(request, deadline.Token)); }
            catch (Exception error) { completion.TrySetException(error); }
        }) { IsBackground = true, Name = "KYNXA desktop operation" };
        // UIA runs on its own MTA thread, never an application's UI thread.
        worker.SetApartmentState(ApartmentState.MTA); worker.Start();
        try { return await completion.Task.WaitAsync(TimeSpan.FromMilliseconds(timeoutMs + 100), cancellation); }
        catch (Exception error) when (request.Action == "read" && !cancellation.IsCancellationRequested &&
            (error is TimeoutException || error is OperationCanceledException && deadline.IsCancellationRequested))
        {
            deadline.Cancel();
            // A stalled provider cannot be cancelled in-process. The helper's background thread
            // is discarded when this independent request process exits after its timeout receipt.
            throw new DesktopException("DESKTOP_READ_TIMEOUT", "The bounded UI Automation read timed out; no input was sent.");
        }
    }

    private static Dictionary<string, object?> Run(DesktopRequest request, CancellationToken cancellation)
    {
        if (request.Operation != "desktop" || request.Action is null || !Operations.Contains(request.Action))
            throw new DesktopException("DESKTOP_INVALID_REQUEST", "Unknown desktop operation.");
        if (!DesktopTarget.IsInteractive()) throw new DesktopException("DESKTOP_UNAVAILABLE", "The interactive desktop is unavailable or locked.");
        cancellation.ThrowIfCancellationRequested();
        nint originalDpi = SetThreadDpiAwarenessContext((nint)(-4));
        try
        {
            Dictionary<string, object?> result;
            if (request.Action == "apps") result = DesktopApplications.Apps();
            else if (request.Action == "windows") result = Windows(request.ProcessId);
            else
            {
                using var inputLock = new Mutex(false, @"Local\KYNXA.Desktop.Input." + Process.GetCurrentProcess().SessionId);
                bool acquired = false;
                try
                {
                    try { acquired = WaitHandle.WaitAny([inputLock, cancellation.WaitHandle], 5000) == 0; }
                    catch (AbandonedMutexException) { acquired = true; }
                    cancellation.ThrowIfCancellationRequested();
                    if (!acquired) throw new DesktopException("DESKTOP_BUSY", "Another desktop action is in progress.");
                    if (request.Action == "launch") result = DesktopApplications.Launch(request, cancellation);
                    else
                    {
                        DesktopTarget target = DesktopTarget.From(request);
                        if (request.Action == "read") result = DesktopReader.Read(target, request, cancellation);
                        else if (request.Action == "screenshot") result = DesktopReader.Screenshot(target, request.Crop);
                        else if (request.Action == "window") result = DesktopWindowState.Apply(target, request);
                        else if (request.Action == "activate") result = Activate(target);
                        else result = DesktopInput.Apply(target, request, cancellation);
                    }
                }
                finally { if (acquired) inputLock.ReleaseMutex(); }
            }
            result["protocolVersion"] = 1; result["boundary"] = "host-desktop";
            result["action"] = request.Action; result["completed"] = true;
            return result;
        }
        finally { if (originalDpi != 0) SetThreadDpiAwarenessContext(originalDpi); }
    }

    private static Dictionary<string, object?> Windows(int? processId)
    {
        if (processId is <= 0) throw new DesktopException("DESKTOP_INVALID_REQUEST", "Process filter must be positive.");
        var windows = new List<DesktopWindow>();
        bool truncated = false;
        EnumWindows((window, _) =>
        {
            GetWindowThreadProcessId(window, out uint owner);
            // Filter before reading titles. Tests inspect only their own synthetic process.
            if (IsWindowVisible(window) && (processId is null || owner == processId))
            {
                if (windows.Count >= 200) { truncated = true; return false; }
                windows.Add(new DesktopTarget(window, (int)owner).Describe());
            }
            return true;
        }, 0);
        return new() { ["windows"] = windows, ["truncated"] = truncated };
    }

    private static Dictionary<string, object?> Activate(DesktopTarget target)
    {
        target.Check(visible: false); target.EnsureIntegrity();
        if (IsIconic(target.Window)) ShowWindow(target.Window, 9);
        if (GetForegroundWindow() != target.Window && !SetForegroundWindow(target.Window))
            throw new DesktopException("DESKTOP_ACTIVATION_BLOCKED", "Windows denied foreground activation; no focus restriction was bypassed.");
        target.Check(foreground: true);
        return new() { ["windowId"] = target.Window.ToInt64().ToString(), ["processId"] = target.ProcessId, ["activated"] = true };
    }
}
