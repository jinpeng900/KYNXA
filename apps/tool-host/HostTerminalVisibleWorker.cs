using System.Diagnostics;
using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using static KYNXA.ToolHost.HostTerminalConsole;

namespace KYNXA.ToolHost;

/// <summary>A bundled companion attached to the command's real console, with protocol traffic on a private pipe.</summary>
/// <remarks>随包提供的辅助进程连接命令的真实控制台，协议数据仅走私有管道。</remarks>
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
            IntPtr consoleWindowHandle = IntPtr.Zero;
            var visibilityWait = Stopwatch.StartNew();
            while (consoleWindowHandle == IntPtr.Zero && visibilityWait.ElapsedMilliseconds < 4000)
            {
                consoleWindowHandle = FindVisibleWindow(title);
                if (consoleWindowHandle == IntPtr.Zero) await Task.Delay(25);
            }
            if (consoleWindowHandle == IntPtr.Zero)
                throw new HostTerminalException("HOST_TERMINAL_VISIBLE_UNAVAILABLE", "No actual visible console window was observed; the script was not executed.");
            await SendAsync(writer, new { @event = "window_ready", windowId = consoleWindowHandle.ToInt64().ToString(),
                windowObserved = IsVisible(consoleWindowHandle), windowProcessId = WindowProcessId(consoleWindowHandle), consoleInput = true, consoleOutput = true });
            if (await reader.ReadLineAsync() != "execute") return 126;

            Console.OutputEncoding = new UTF8Encoding(false);
            Console.WriteLine("KYNXA visible host terminal");
            var processStartInfo = new ProcessStartInfo(HostTerminalRunner.ShellPath(request.Shell!))
            {
                WorkingDirectory = request.Cwd!, UseShellExecute = false, CreateNoWindow = false
                // No redirection: the shell and its children inherit genuine console input/output.
                // 不重定向输入输出，让 shell 及其子进程继承真实控制台。
            };
            if (request.Shell == "cmd")
            {
                // CMD /s /c parses its outer quotes itself; argv escaping would insert literal
                // backslashes before the script's executable quotes and prevent command startup.
                // CMD /s /c 自行解析外层引号；argv 转义会在脚本可执行路径的引号前插入字面反斜杠，导致命令无法启动。
                processStartInfo.Arguments = " /d /s /c \"" + request.Script! + "\"";
            }
            else
            {
                string scriptEnvironmentVariable = "KYNXA_HOST_SOURCE_" + Guid.NewGuid().ToString("N");
                processStartInfo.Environment[scriptEnvironmentVariable] = request.Script!;
                string shellWrapper = "$kynxaHostScript=[Environment]::GetEnvironmentVariable('" + scriptEnvironmentVariable + "','Process');" +
                    "[Environment]::SetEnvironmentVariable('" + scriptEnvironmentVariable + "',$null,'Process');" +
                    "& ([ScriptBlock]::Create($kynxaHostScript))";
                processStartInfo.ArgumentList.Add("-NoProfile"); processStartInfo.ArgumentList.Add("-Command"); processStartInfo.ArgumentList.Add(shellWrapper);
            }
            using Process shellProcess = Process.Start(processStartInfo)
                ?? throw new HostTerminalException("HOST_TERMINAL_START_FAILED", "The visible shell could not start.");
            commandStarted = true;
            await SendAsync(writer, new
            {
                @event = "command_started", processId = shellProcess.Id, workerProcessId = Environment.ProcessId,
                windowId = consoleWindowHandle.ToInt64().ToString(), windowObserved = IsVisible(consoleWindowHandle), windowProcessId = WindowProcessId(consoleWindowHandle),
                consoleInput = true, consoleOutput = true
            });
            Task commandExit = shellProcess.WaitForExitAsync();
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
            Console.WriteLine($"\n[KYNXA] Command exited with code {shellProcess.ExitCode}.");
            if (displayHoldMs != 0)
                Console.WriteLine($"[KYNXA] This window will close in {displayHoldMs / 1000.0:0.#} seconds.");
            var snapshot = Snapshot();
            await SendAsync(writer, new
            {
                @event = "command_completed", exitCode = shellProcess.ExitCode, consoleText = snapshot.Text,
                consoleSnapshotTruncated = snapshot.Truncated, consoleSnapshotAvailable = snapshot.Available,
                windowVisibleAtCompletion = IsVisible(consoleWindowHandle)
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
