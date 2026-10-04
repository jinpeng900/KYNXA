using System.Diagnostics;
using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using static KYNXA.ToolHost.HostTerminalConsole;

namespace KYNXA.ToolHost;

/// <summary>A bundled companion attached to the command's real console, with protocol traffic on a private pipe.</summary>
internal static class HostTerminalVisibleWorker
{
    internal static async Task<int> RunAsync(string pipeName)
    {
        if (!pipeName.StartsWith("kynxa-visible-", StringComparison.Ordinal) || pipeName.Length > 80) return 126;
        using var pipe = new NamedPipeClientStream(".", pipeName, PipeDirection.InOut, PipeOptions.Asynchronous);
        using var connectionDeadline = new CancellationTokenSource(5000);
        try { await pipe.ConnectAsync(connectionDeadline.Token); }
        catch (Exception error) when (error is IOException or OperationCanceledException) { return 126; }
        using var reader = new StreamReader(pipe, new UTF8Encoding(false), leaveOpen: true);
        using var writer = new StreamWriter(pipe, new UTF8Encoding(false), leaveOpen: true) { AutoFlush = true };
        bool commandStarted = false;
        try
        {
            await SendAsync(writer, new { @event = "worker_ready", processId = Environment.ProcessId });
            string? input = await reader.ReadLineAsync(connectionDeadline.Token);
            if (input is null || input.Length > 128 * 1024) throw new HostTerminalException("HOST_TERMINAL_INVALID_REQUEST", "Invalid visible worker request.");
            var request = JsonSerializer.Deserialize<HostTerminalRequest>(input, Program.JsonOptions)
                ?? throw new HostTerminalException("HOST_TERMINAL_INVALID_REQUEST", "Missing visible worker request.");
            int displayHoldMs = request.KeepOpenMs ?? 5000;
            string title = "KYNXA terminal " + pipeName["kynxa-visible-".Length..];
            SetTitle(title);
            if (!HasRealConsoleHandles())
                throw new HostTerminalException("HOST_TERMINAL_VISIBLE_UNAVAILABLE", "The visible worker has no real console input/output handles.");
            IntPtr window = IntPtr.Zero;
            var visibilityWait = Stopwatch.StartNew();
            while (window == IntPtr.Zero && visibilityWait.ElapsedMilliseconds < 4000)
            {
                window = FindVisibleWindow(title);
                if (window == IntPtr.Zero) await Task.Delay(25);
            }
            if (window == IntPtr.Zero)
                throw new HostTerminalException("HOST_TERMINAL_VISIBLE_UNAVAILABLE", "No actual visible console window was observed; the script was not executed.");
            await SendAsync(writer, new { @event = "window_ready", windowId = window.ToInt64().ToString(),
                windowObserved = IsVisible(window), windowProcessId = WindowProcessId(window), consoleInput = true, consoleOutput = true });
            if (await reader.ReadLineAsync() != "execute") return 126;

            Console.OutputEncoding = new UTF8Encoding(false);
            Console.WriteLine("KYNXA visible host terminal");
            var start = new ProcessStartInfo(HostTerminalRunner.ShellPath(request.Shell!))
            {
                WorkingDirectory = request.Cwd!, UseShellExecute = false, CreateNoWindow = false
                // No redirection: the shell and its children inherit genuine console input/output.
            };
            if (request.Shell == "cmd")
            {
                // CMD /s /c parses its outer quotes itself; argv escaping would insert literal
                // backslashes before the script's executable quotes and prevent command startup.
                start.Arguments = " /d /s /c \"" + request.Script! + "\"";
            }
            else
            {
                string source = "KYNXA_HOST_SOURCE_" + Guid.NewGuid().ToString("N");
                start.Environment[source] = request.Script!;
                string wrapper = "$kynxaHostScript=[Environment]::GetEnvironmentVariable('" + source + "','Process');" +
                    "[Environment]::SetEnvironmentVariable('" + source + "',$null,'Process');" +
                    "& ([ScriptBlock]::Create($kynxaHostScript))";
                start.ArgumentList.Add("-NoProfile"); start.ArgumentList.Add("-Command"); start.ArgumentList.Add(wrapper);
            }
            using Process process = Process.Start(start)
                ?? throw new HostTerminalException("HOST_TERMINAL_START_FAILED", "The visible shell could not start.");
            commandStarted = true;
            await SendAsync(writer, new
            {
                @event = "command_started", processId = process.Id, workerProcessId = Environment.ProcessId,
                windowId = window.ToInt64().ToString(), windowObserved = IsVisible(window), windowProcessId = WindowProcessId(window),
                consoleInput = true, consoleOutput = true
            });
            Task commandExit = process.WaitForExitAsync();
            string? lastSnapshotText = null;
            bool lastSnapshotTruncated = false;
            while (!commandExit.IsCompleted)
            {
                await Task.WhenAny(commandExit, Task.Delay(150));
                if (commandExit.IsCompleted) break;
                var preview = Snapshot();
                if (preview.Available && preview.Text == lastSnapshotText && preview.Truncated == lastSnapshotTruncated) continue;
                if (preview.Available) { lastSnapshotText = preview.Text; lastSnapshotTruncated = preview.Truncated; }
                await SendAsync(writer, new { @event = "console_snapshot", consoleText = preview.Text,
                    consoleSnapshotTruncated = preview.Truncated, consoleSnapshotAvailable = preview.Available });
            }
            await commandExit;
            Console.WriteLine($"\n[KYNXA] Command exited with code {process.ExitCode}.");
            if (displayHoldMs != 0)
                Console.WriteLine($"[KYNXA] This window will close in {displayHoldMs / 1000.0:0.#} seconds.");
            var snapshot = Snapshot();
            await SendAsync(writer, new
            {
                @event = "command_completed", exitCode = process.ExitCode, consoleText = snapshot.Text,
                consoleSnapshotTruncated = snapshot.Truncated, consoleSnapshotAvailable = snapshot.Available,
                windowVisibleAtCompletion = IsVisible(window)
            });
            if (displayHoldMs != 0) await Task.Delay(displayHoldMs);
            return 0;
        }
        catch (Exception error)
        {
            try
            {
                await SendAsync(writer, new { @event = "worker_failed", commandStarted,
                    error = new { code = error is HostTerminalException failure ? failure.Code : "HOST_TERMINAL_FAILED", message = error.Message } });
            }
            catch (IOException) { }
            return 126;
        }
    }

    private static Task SendAsync(StreamWriter writer, object value) =>
        writer.WriteLineAsync(JsonSerializer.Serialize(value, Program.JsonOptions));
}
