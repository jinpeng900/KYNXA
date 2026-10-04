using System.Diagnostics;
using System.IO.Pipes;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using static KYNXA.ToolHost.HostTerminalConsole;
using static KYNXA.ToolHost.NativeMethods;

namespace KYNXA.ToolHost;

/// <summary>Owns the real-console companion and its complete bounded process tree.</summary>
internal static class HostTerminalVisibleRunner
{
    private const int MaximumFrameCharacters = 768 * 1024;

    internal static async Task<object> RunAsync(HostTerminalRequest request, CancellationToken cancellationToken)
    {
        string pipeName = "kynxa-visible-" + Guid.NewGuid().ToString("N");
        using var pipe = new NamedPipeServerStream(pipeName, PipeDirection.InOut, 1,
            PipeTransmissionMode.Byte, PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly);
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(request.TimeoutMs);
        IntPtr job = IntPtr.Zero;
        var worker = new ProcessInformation();
        bool executionAuthorized = false, commandStarted = false, commandCompleted = false, cancelled = false, timedOut = false;
        int commandProcessId = 0, windowProcessId = 0, exitCode = 125;
        string? windowId = null;
        string consoleText = "";
        bool consoleSnapshotTruncated = false, consoleSnapshotAvailable = false, windowVisibleAtCompletion = false;
        long commandCompletedAtMs = 0;
        int outputSequence = 0;
        string? displayError = null;
        var elapsed = Stopwatch.StartNew();
        var placement = new DesktopForegroundPlacement();
        try
        {
            job = CreateJobObjectW(IntPtr.Zero, null);
            HostTerminalConsole.Check(job != IntPtr.Zero, "Create visible terminal job");
            var limits = new JobExtendedLimitInformation
            {
                BasicLimitInformation = new JobBasicLimitInformation { LimitFlags = 0x2000 | 0x8, ActiveProcessLimit = 32 }
            };
            HostTerminalConsole.Check(SetInformationJobObject(job, 9, ref limits, (uint)Marshal.SizeOf<JobExtendedLimitInformation>()), "Set visible terminal job limits");
            string executable = Path.Combine(AppContext.BaseDirectory, "KYNXA.ToolHost.exe");
            if (!File.Exists(executable))
                throw new HostTerminalException("HOST_TERMINAL_VISIBLE_UNAVAILABLE", "The bundled console companion executable is unavailable.");
            var startup = new StartupInfoEx { StartupInfo = new StartupInfo
            {
                Cb = Marshal.SizeOf<StartupInfo>(), Flags = 1, ShowWindow = 4 // STARTF_USESHOWWINDOW / SW_SHOWNOACTIVATE.
            } };
            string arguments = HostTerminalRunner.QuoteArgument(executable) + " --host-terminal-visible-worker " + pipeName;
            HostTerminalConsole.Check(CreateProcessW(executable, new StringBuilder(arguments), IntPtr.Zero, IntPtr.Zero, false,
                CreateSuspended | 0x10, IntPtr.Zero, request.Cwd!, ref startup, out worker), "Create visible terminal companion");
            try
            {
                HostTerminalConsole.Check(AssignProcessToJobObject(job, worker.Process), "Assign visible terminal companion to job");
                deadline.Token.ThrowIfCancellationRequested();
                if (ResumeThread(worker.Thread) == uint.MaxValue) HostTerminalConsole.Check(false, "Resume visible terminal companion");
            }
            catch { TerminateProcess(worker.Process, 125); throw; }
            await pipe.WaitForConnectionAsync(deadline.Token);
            HostTerminalConsole.Check(GetNamedPipeClientProcessId(pipe.SafePipeHandle.DangerousGetHandle(), out uint clientProcessId), "Verify visible worker pipe client");
            if (clientProcessId != worker.ProcessId)
                throw new HostTerminalException("HOST_TERMINAL_VISIBLE_UNAVAILABLE", "Unexpected visible worker pipe client.");
            using var reader = new StreamReader(pipe, new UTF8Encoding(false), leaveOpen: true);
            using var writer = new StreamWriter(pipe, new UTF8Encoding(false), leaveOpen: true) { AutoFlush = true };
            using (JsonDocument hello = await ReadRecordAsync(reader, deadline.Token)
                ?? throw new HostTerminalException("HOST_TERMINAL_VISIBLE_UNAVAILABLE", "The visible companion closed before handshake."))
            {
                if (hello.RootElement.GetProperty("event").GetString() != "worker_ready" ||
                    hello.RootElement.GetProperty("processId").GetInt32() != worker.ProcessId)
                    throw new HostTerminalException("HOST_TERMINAL_VISIBLE_UNAVAILABLE", "Invalid visible companion handshake.");
            }
            await writer.WriteLineAsync(JsonSerializer.Serialize(request, Program.JsonOptions));
            while (true)
            {
                using JsonDocument? record = await ReadRecordAsync(reader, deadline.Token);
                if (record is null) break;
                JsonElement value = record.RootElement;
                string? name = value.GetProperty("event").GetString();
                if (name == "window_ready")
                {
                    windowId = value.GetProperty("windowId").GetString();
                    windowProcessId = value.GetProperty("windowProcessId").GetInt32();
                    if (!long.TryParse(windowId, out long handle) || !IsVisible((IntPtr)handle) ||
                        !value.GetProperty("consoleInput").GetBoolean() || !value.GetProperty("consoleOutput").GetBoolean())
                        throw new HostTerminalException("HOST_TERMINAL_VISIBLE_UNAVAILABLE", "The visible console could not be verified; script not executed.");
                    placement.PlaceBehind((nint)handle, windowProcessId);
                    deadline.Token.ThrowIfCancellationRequested();
                    executionAuthorized = true;
                    await writer.WriteLineAsync("execute");
                }
                else if (name == "command_started")
                {
                    commandStarted = true;
                    commandProcessId = value.GetProperty("processId").GetInt32();
                    windowId = value.GetProperty("windowId").GetString();
                    if (!long.TryParse(windowId, out long handle) || !IsVisible((IntPtr)handle) ||
                        !value.GetProperty("consoleInput").GetBoolean() || !value.GetProperty("consoleOutput").GetBoolean())
                        throw new HostTerminalException("HOST_TERMINAL_VISIBLE_UNAVAILABLE", "The visible console could not be verified after command start.", true);
                    placement.PlaceBehind((nint)handle, windowProcessId);
                    Console.WriteLine(JsonSerializer.Serialize(new
                    {
                        protocolVersion = 2, boundary = "host-terminal", @event = "host_terminal_started",
                        processId = commandProcessId, workerProcessId = worker.ProcessId,
                        visibleRequested = true, windowObserved = true, windowId, windowProcessId,
                        backgroundRequested = true, backgroundMode = "best-effort-no-activate",
                        foregroundPreserved = placement.ForegroundPreserved,
                        backgroundPlacementConfirmed = placement.WindowPlacedBehind
                    }, Program.JsonOptions));
                }
                else if (name is "command_completed" or "console_snapshot" && commandStarted)
                {
                    if (name == "command_completed")
                    {
                        exitCode = value.GetProperty("exitCode").GetInt32();
                        windowVisibleAtCompletion = value.GetProperty("windowVisibleAtCompletion").GetBoolean();
                        // Authoritative completion precedes optional preview publication and display hold.
                        commandCompleted = true;
                        commandCompletedAtMs = elapsed.ElapsedMilliseconds;
                    }
                    string preview = value.GetProperty("consoleText").GetString() ?? "";
                    if (Encoding.UTF8.GetByteCount(preview) > 256 * 1024)
                        throw new HostTerminalException("HOST_TERMINAL_FAILED", "The console snapshot exceeded its byte bound.", true);
                    if (value.GetProperty("consoleSnapshotAvailable").GetBoolean())
                    {
                        consoleText = preview;
                        consoleSnapshotTruncated = value.GetProperty("consoleSnapshotTruncated").GetBoolean();
                        consoleSnapshotAvailable = true;
                        Console.WriteLine(JsonSerializer.Serialize(new
                        {
                            protocolVersion = 2, boundary = "host-terminal", @event = "host_terminal_output",
                            stream = "console", text = consoleText, replace = true, sequence = ++outputSequence
                        }, Program.JsonOptions));
                    }
                }
                else if (name == "worker_failed")
                {
                    JsonElement error = value.GetProperty("error");
                    throw new HostTerminalException(error.GetProperty("code").GetString() ?? "HOST_TERMINAL_FAILED",
                        error.GetProperty("message").GetString() ?? "The visible companion failed.",
                        commandStarted || value.GetProperty("commandStarted").GetBoolean());
                }
            }
        }
        catch (OperationCanceledException)
        {
            cancelled = cancellationToken.IsCancellationRequested;
            timedOut = !cancelled;
        }
        catch (IOException) when (executionAuthorized && !commandCompleted) { }
        catch (Exception error) when (commandCompleted) { displayError = error.Message; }
        catch (HostTerminalException error) when (executionAuthorized && !error.ProcessStarted)
        {
            throw new HostTerminalException(error.Code, error.Message, true);
        }
        catch (Exception error) when (executionAuthorized)
        {
            throw new HostTerminalException("HOST_TERMINAL_FAILED", error.Message, true);
        }
        finally
        {
            if (job != IntPtr.Zero)
            {
                TerminateJobObject(job, timedOut ? 124u : 125u);
                try { await HostTerminalRunner.WaitForEmptyJobAsync(job); }
                finally { Close(ref worker.Thread); Close(ref worker.Process); Close(ref job); }
            }
        }
        if (!commandStarted)
            throw new HostTerminalException(cancelled ? "HOST_TERMINAL_CANCELLED" : "HOST_TERMINAL_VISIBLE_UNAVAILABLE",
                "The visible terminal did not return a command-start acknowledgement.", executionAuthorized);
        return new
        {
            protocolVersion = 2, boundary = "host-terminal", completed = commandCompleted,
            outcome = commandCompleted ? "completed" : "unknown", commandCompleted,
            shell = request.Shell!, cwd = Path.GetFullPath(request.Cwd!), exitCode, stdout = "", stderr = "",
            timedOut, cancelled, processId = commandProcessId, workerProcessId = worker.ProcessId,
            activeProcessesAfterExit = 0, visibleRequested = true, windowObserved = true, windowId, windowProcessId,
            backgroundRequested = true, backgroundMode = "best-effort-no-activate",
            foregroundPreserved = placement.ForegroundPreserved,
            focusRestoreAttempted = placement.FocusRestoreAttempted, focusRestoreSucceeded = placement.FocusRestoreSucceeded,
            windowVisibleAtCompletion, consoleInput = "console", consoleOutput = "console", outputCapture = "console-screen",
            consoleText, consoleSnapshotTruncated, consoleSnapshotAvailable, transcriptComplete = false, streamsSeparated = false,
            exitCodeObserved = commandCompleted,
            outputTruncated = consoleSnapshotTruncated, outputLimitExceeded = false,
            maxOutputBytes = 256 * 1024, capturedOutputBytes = Encoding.UTF8.GetByteCount(consoleText),
            displayHoldRequestedMs = request.KeepOpenMs ?? 5000,
            displayHoldMs = commandCompleted ? (int)Math.Clamp(elapsed.ElapsedMilliseconds - commandCompletedAtMs, 0, request.KeepOpenMs ?? 5000) : 0,
            displayClosedEarly = commandCompleted && (cancelled || timedOut || displayError is not null ||
                elapsed.ElapsedMilliseconds - commandCompletedAtMs + 50 < (request.KeepOpenMs ?? 5000)), displayError,
            elapsedMs = elapsed.ElapsedMilliseconds, outputEncoding = "console-unicode",
            runtimeArguments = request.Shell == "cmd" ? new[] { "/d", "/s", "/c" } : new[] { "-NoProfile", "-Command" }
        };
    }

    private static async Task<JsonDocument?> ReadRecordAsync(StreamReader reader, CancellationToken cancellationToken)
    {
        string? line = await reader.ReadLineAsync(cancellationToken);
        if (line is null) return null;
        if (line.Length > MaximumFrameCharacters)
            throw new HostTerminalException("HOST_TERMINAL_FAILED", "Visible companion frame exceeds its bound.");
        return JsonDocument.Parse(line);
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetNamedPipeClientProcessId(IntPtr pipe, out uint processId);
}
