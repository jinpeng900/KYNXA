using Microsoft.Win32.SafeHandles;
using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using static KYNXA.ToolHost.NativeMethods;

namespace KYNXA.ToolHost;

/// <summary>Explicit host execution. This channel never changes the AppContainer terminal contract.</summary>
internal static class HostTerminalRunner
{
    private const int MaximumOutputBytes = 256 * 1024;
    private const int MaximumProcesses = 32;

    internal static object Capabilities()
    {
        string[] shells = OperatingSystem.IsWindows()
            ? new[] { "cmd", "powershell" }.Where(shell => File.Exists(ShellPath(shell))).ToArray() : [];
        return new
        {
            protocolVersion = 2, boundary = "host-terminal", available = shells.Length != 0, shells,
            sandbox = false, processTreeBounded = true, maxOutputBytes = MaximumOutputBytes,
            maximumProcesses = MaximumProcesses, minimumTimeoutMs = 100, maximumTimeoutMs = 120000,
            maxScriptCharacters = HostTerminalRequest.MaximumScriptCharacters,
            visibleTerminal = true, maxKeepOpenMs = 30000, defaultKeepOpenMs = 5000,
            outputEvents = true,
            visibleOutput = "bounded-console-screen-snapshot",
            outputEncoding = new { cmd = "utf-8-or-oem-per-line", powershell = "utf-8" }
        };
    }

    internal static async Task<object> RunAsync(HostTerminalRequest request, CancellationToken cancellationToken)
    {
        Validate(request);
        cancellationToken.ThrowIfCancellationRequested();
        if (request.Visible) return await HostTerminalVisibleRunner.RunAsync(request, cancellationToken);
        string cwd = Path.GetFullPath(request.Cwd!);
        string shell = request.Shell!;
        IntPtr job = IntPtr.Zero;
        var process = new ProcessInformation();
        bool started = false;
        var elapsed = Stopwatch.StartNew();
        try
        {
            job = CreateJobObjectW(IntPtr.Zero, null);
            Check(job != IntPtr.Zero, "Create host terminal job");
            var limits = new JobExtendedLimitInformation
            {
                BasicLimitInformation = new JobBasicLimitInformation
                {
                    // Kill-on-close applies even if the gateway or this helper is terminated.
                    // No child can break out of the job; a process starts suspended until assignment.
                    LimitFlags = 0x2000 | 0x8, ActiveProcessLimit = MaximumProcesses
                }
            };
            Check(SetInformationJobObject(job, 9, ref limits, (uint)Marshal.SizeOf<JobExtendedLimitInformation>()), "Set host terminal job limits");
            var output = new BoundedOutput(job, shell == "cmd");
            var (stdout, stderr) = Start(shell, request.Script!, cwd, job, ref process, output, cancellationToken);
            started = true;
            Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(new
            {
                protocolVersion = 1, boundary = "host-terminal", @event = "host_terminal_started", processId = process.ProcessId
            }, Program.JsonOptions));
            output.StartEvents();

            bool timedOut = false, cancelled = false, normalExit = false;
            while (true)
            {
                uint wait = WaitForSingleObject(process.Process, 25);
                if (wait == 0) { normalExit = true; break; }
                Check(wait == WaitTimeout, "Wait for host terminal process");
                if (cancellationToken.IsCancellationRequested) { cancelled = true; break; }
                if (elapsed.ElapsedMilliseconds >= request.TimeoutMs) { timedOut = true; break; }
                await Task.Delay(10);
            }
            // Kill surviving descendants after both interrupted and normal shell completion.
            Check(TerminateJobObject(job, timedOut ? 124u : 125u), "Terminate host terminal process tree");
            await WaitForEmptyJobAsync(job);
            Check(GetExitCodeProcess(process.Process, out uint exitCode), "Read host terminal exit code");
            await Task.WhenAll(stdout, stderr);
            bool completed = normalExit && !output.Truncated;
            return new
            {
                protocolVersion = 1, boundary = "host-terminal", completed, outcome = completed ? "completed" : "unknown",
                shell, cwd, exitCode = unchecked((int)exitCode), stdout = await stdout, stderr = await stderr,
                timedOut, cancelled, processId = process.ProcessId, activeProcessesAfterExit = 0,
                outputTruncated = output.Truncated, outputLimitExceeded = output.Truncated,
                elapsedMs = elapsed.ElapsedMilliseconds, maxOutputBytes = MaximumOutputBytes, capturedOutputBytes = output.CapturedBytes,
                outputEncoding = shell == "cmd" ? "utf-8-or-oem-per-line" : "utf-8",
                runtimeArguments = shell == "cmd" ? new[] { "/d", "/s", "/c" } : new[] { "-NoProfile", "-NonInteractive", "-Command" }
            };
        }
        catch (HostTerminalException error) when (started && !error.ProcessStarted)
        {
            throw new HostTerminalException(error.Code, error.Message, processStarted: true);
        }
        catch (Exception error) when (started && error is not HostTerminalException)
        {
            throw new HostTerminalException("HOST_TERMINAL_FAILED", error.Message, processStarted: true);
        }
        finally
        {
            if (job != IntPtr.Zero) TerminateJobObject(job, 125);
            Close(ref process.Thread);
            Close(ref process.Process);
            Close(ref job);
        }
    }

    private static void Validate(HostTerminalRequest request)
    {
        if (!OperatingSystem.IsWindows()) throw new HostTerminalException("HOST_TERMINAL_UNAVAILABLE", "Host terminal execution requires Windows.");
        if (request.Operation is not ("host_terminal" or "host_terminal_visible") ||
            (request.Operation == "host_terminal_visible" && !request.Visible) || request.Shell is not ("cmd" or "powershell"))
            throw new HostTerminalException("HOST_TERMINAL_INVALID_REQUEST", "The host terminal accepts only cmd or powershell.");
        if (string.IsNullOrWhiteSpace(request.Script) || request.Script.Length > HostTerminalRequest.MaximumScriptCharacters || request.Script.Contains('\0'))
            throw new HostTerminalException("HOST_TERMINAL_INVALID_REQUEST", "A bounded nonempty script without NUL characters is required.");
        if (string.IsNullOrWhiteSpace(request.Cwd) || !Path.IsPathFullyQualified(request.Cwd) || request.Cwd.StartsWith("\\\\", StringComparison.Ordinal) ||
            !Directory.Exists(request.Cwd))
            throw new HostTerminalException("HOST_TERMINAL_INVALID_WORKSPACE", "An existing absolute local working directory is required.");
        if (request.TimeoutMs is < 100 or > 120000)
            throw new HostTerminalException("HOST_TERMINAL_INVALID_REQUEST", "Timeout must be between 100 and 120000 milliseconds.");
        if (request.KeepOpenMs is < 0 or > 30000 || (!request.Visible && request.KeepOpenMs.HasValue))
            throw new HostTerminalException("HOST_TERMINAL_INVALID_REQUEST", "Keep-open must be between 0 and 30000 milliseconds and requires visible mode.");
        string executable = ShellPath(request.Shell);
        if (!File.Exists(executable) || (File.GetAttributes(executable) & FileAttributes.ReparsePoint) != 0)
            throw new HostTerminalException("HOST_TERMINAL_UNAVAILABLE", "The pinned Windows shell is unavailable.");
    }

    internal static string ShellPath(string shell) => shell == "cmd"
        ? Path.Combine(Environment.SystemDirectory, "cmd.exe")
        : Path.Combine(Environment.SystemDirectory, "WindowsPowerShell", "v1.0", "powershell.exe");

    private static (Task<string> Stdout, Task<string> Stderr) Start(string shell, string script, string cwd, IntPtr job,
        ref ProcessInformation process, BoundedOutput output, CancellationToken cancellationToken)
    {
        IntPtr stdoutRead = IntPtr.Zero, stdoutWrite = IntPtr.Zero, stderrRead = IntPtr.Zero, stderrWrite = IntPtr.Zero;
        IntPtr stdinRead = IntPtr.Zero, stdinWrite = IntPtr.Zero, attributes = IntPtr.Zero, handlesBuffer = IntPtr.Zero, environment = IntPtr.Zero;
        bool resumed = false;
        try
        {
            var security = new SecurityAttributes { Length = Marshal.SizeOf<SecurityAttributes>(), InheritHandle = 1 };
            Check(CreatePipe(out stdoutRead, out stdoutWrite, ref security, 0), "Create host stdout pipe");
            Check(CreatePipe(out stderrRead, out stderrWrite, ref security, 0), "Create host stderr pipe");
            Check(CreatePipe(out stdinRead, out stdinWrite, ref security, 0), "Create host stdin pipe");
            Check(SetHandleInformation(stdoutRead, 1, 0), "Protect host stdout reader");
            Check(SetHandleInformation(stderrRead, 1, 0), "Protect host stderr reader");
            Check(SetHandleInformation(stdinWrite, 1, 0), "Protect host stdin writer");
            UIntPtr size = UIntPtr.Zero;
            InitializeProcThreadAttributeList(IntPtr.Zero, 1, 0, ref size);
            attributes = Marshal.AllocHGlobal(checked((int)size.ToUInt64()));
            Check(InitializeProcThreadAttributeList(attributes, 1, 0, ref size), "Initialize host process attributes");
            handlesBuffer = Marshal.AllocHGlobal(IntPtr.Size * 3);
            Marshal.WriteIntPtr(handlesBuffer, stdoutWrite);
            Marshal.WriteIntPtr(handlesBuffer, IntPtr.Size, stderrWrite);
            Marshal.WriteIntPtr(handlesBuffer, IntPtr.Size * 2, stdinRead);
            Check(UpdateProcThreadAttribute(attributes, 0, (IntPtr)0x20002, handlesBuffer, (UIntPtr)(IntPtr.Size * 3), IntPtr.Zero, IntPtr.Zero), "Set host inherited handle allowlist");
            var startup = new StartupInfoEx
            {
                AttributeList = attributes,
                StartupInfo = new StartupInfo
                {
                    Cb = Marshal.SizeOf<StartupInfoEx>(), Flags = StartfUseStdHandles,
                    StdInput = stdinRead, StdOutput = stdoutWrite, StdError = stderrWrite
                }
            };
            string executable = ShellPath(shell);
            string sourceVariable = "KYNXA_HOST_SOURCE_" + Guid.NewGuid().ToString("N");
            string powershellWrapper = "[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false);$OutputEncoding=[Console]::OutputEncoding;" +
                "$kynxaHostScript=[Environment]::GetEnvironmentVariable('" + sourceVariable + "','Process');" +
                "[Environment]::SetEnvironmentVariable('" + sourceVariable + "',$null,'Process');" +
                "& ([ScriptBlock]::Create($kynxaHostScript))";
            string arguments = shell == "cmd"
                ? QuoteArgument(executable) + " /d /s /c \"" + script + "\""
                : QuoteArgument(executable) + " -NoProfile -NonInteractive -Command " + QuoteArgument(powershellWrapper);
            if (arguments.Length >= 32767)
                throw new HostTerminalException("HOST_TERMINAL_INVALID_REQUEST", "The effective shell command line exceeds the Windows limit.");
            // The original script remains a separately parsed script block: leading using/param syntax is preserved.
            // A private, per-child source variable also avoids command-line expansion of a 16K quoted script.
            if (shell == "powershell") environment = Marshal.StringToHGlobalUni(BuildEnvironment(sourceVariable, script));
            cancellationToken.ThrowIfCancellationRequested();
            // Host execution preserves the application's environment, including user-configured application PATH.
            Check(CreateProcessW(executable, new StringBuilder(arguments), IntPtr.Zero, IntPtr.Zero, true,
                CreateSuspended | ExtendedStartupInfoPresent | CreateNoWindow | (environment != IntPtr.Zero ? CreateUnicodeEnvironment : 0),
                environment, cwd, ref startup, out process), "Create host terminal process");
            try
            {
                Check(AssignProcessToJobObject(job, process.Process), "Assign host terminal process to job");
                cancellationToken.ThrowIfCancellationRequested();
                if (ResumeThread(process.Thread) == uint.MaxValue) Check(false, "Resume host terminal process");
                resumed = true;
            }
            catch { TerminateProcess(process.Process, 125); throw; }
            Close(ref stdoutWrite); Close(ref stderrWrite); Close(ref stdinRead); Close(ref stdinWrite);
            var stdout = output.ReadAsync(stdoutRead, "stdout"); stdoutRead = IntPtr.Zero;
            var stderr = output.ReadAsync(stderrRead, "stderr"); stderrRead = IntPtr.Zero;
            return (stdout, stderr);
        }
        catch (Exception error) when (resumed)
        {
            throw new HostTerminalException("HOST_TERMINAL_FAILED", error.Message, processStarted: true);
        }
        finally
        {
            Close(ref stdoutRead); Close(ref stdoutWrite); Close(ref stderrRead); Close(ref stderrWrite);
            Close(ref stdinRead); Close(ref stdinWrite);
            if (attributes != IntPtr.Zero) { DeleteProcThreadAttributeList(attributes); Marshal.FreeHGlobal(attributes); }
            if (handlesBuffer != IntPtr.Zero) Marshal.FreeHGlobal(handlesBuffer);
            if (environment != IntPtr.Zero) Marshal.FreeHGlobal(environment);
        }
    }

    internal static string BuildEnvironment(string sourceVariable, string script)
    {
        var values = new SortedDictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        foreach (System.Collections.DictionaryEntry pair in Environment.GetEnvironmentVariables())
            if (pair.Key is string key && pair.Value is string value) values[key] = value;
        values[sourceVariable] = script;
        return string.Join('\0', values.Select(pair => pair.Key + "=" + pair.Value)) + "\0\0";
    }

    internal static string QuoteArgument(string argument)
    {
        var result = new StringBuilder("\"");
        int slashes = 0;
        foreach (char character in argument)
        {
            if (character == '\\') { slashes++; continue; }
            result.Append('\\', character == '"' ? slashes * 2 + 1 : slashes);
            result.Append(character); slashes = 0;
        }
        return result.Append('\\', slashes * 2).Append('"').ToString();
    }

    internal static async Task WaitForEmptyJobAsync(IntPtr job)
    {
        for (int attempt = 0; attempt < 200; attempt++)
        {
            Check(QueryInformationJobObject(job, 1, out var information, (uint)Marshal.SizeOf<JobBasicAccountingInformation>(), IntPtr.Zero), "Query host terminal process tree");
            if (information.ActiveProcesses == 0) return;
            await Task.Delay(10);
        }
        throw new HostTerminalException("HOST_TERMINAL_CLEANUP_FAILED", "The host terminal job still contains live processes after termination.", processStarted: true);
    }

    private static void Check(bool succeeded, string operation)
    {
        if (!succeeded) throw new HostTerminalException("HOST_TERMINAL_START_FAILED", $"{operation}: {new Win32Exception(Marshal.GetLastWin32Error()).Message}");
    }

    private sealed class BoundedOutput(IntPtr job, bool cmdOutput)
    {
        private long _bytes, _capturedBytes;
        private int _truncated;
        private readonly object _eventLock = new();
        private readonly Dictionary<string, StringBuilder> _pendingEvents = new(StringComparer.Ordinal);
        private readonly Stopwatch _eventClock = Stopwatch.StartNew();
        private bool _eventsStarted;
        private int _sequence, _pendingCharacters;
        private long _lastPublishedMs = -1000;
        internal bool Truncated => Volatile.Read(ref _truncated) != 0;
        internal long CapturedBytes => Volatile.Read(ref _capturedBytes);

        internal void StartEvents()
        {
            lock (_eventLock) { _eventsStarted = true; FlushEvents(); }
        }

        internal Task<string> ReadAsync(IntPtr handle, string channel) => Task.Run(() =>
        {
            using var stream = new FileStream(new SafeFileHandle(handle, ownsHandle: true), FileAccess.Read);
            using var stored = new MemoryStream();
            using var line = new MemoryStream();
            byte[] buffer = new byte[4096];
            int count;
            while ((count = stream.Read(buffer)) != 0)
            {
                long total = Interlocked.Add(ref _bytes, count);
                int accepted = (int)Math.Clamp(MaximumOutputBytes - (total - count), 0, count);
                stored.Write(buffer, 0, accepted);
                Interlocked.Add(ref _capturedBytes, accepted);
                var decoded = new StringBuilder();
                for (int index = 0; index < accepted; index++)
                {
                    line.WriteByte(buffer[index]);
                    if (buffer[index] != (byte)'\n') continue;
                    decoded.Append(DecodeOutput(line.ToArray(), cmdOutput));
                    line.SetLength(0);
                    line.Position = 0;
                }
                PublishDelta(channel, decoded.ToString());
                if (total > MaximumOutputBytes && Interlocked.Exchange(ref _truncated, 1) == 0)
                    TerminateJobObject(job, 126);
            }
            if (line.Length != 0) PublishDelta(channel, DecodeOutput(line.ToArray(), cmdOutput));
            lock (_eventLock) FlushEvents();
            return DecodeOutput(stored.ToArray(), cmdOutput);
        });

        private void PublishDelta(string channel, string delta)
        {
            if (delta.Length == 0) return;
            lock (_eventLock)
            {
                if (!_pendingEvents.TryGetValue(channel, out StringBuilder? value))
                    _pendingEvents[channel] = value = new StringBuilder();
                value.Append(delta); _pendingCharacters += delta.Length;
                if (_eventsStarted && (_pendingCharacters >= 4096 || _eventClock.ElapsedMilliseconds - _lastPublishedMs >= 50))
                    FlushEvents();
            }
        }

        private void FlushEvents()
        {
            if (!_eventsStarted) return;
            foreach (var pending in _pendingEvents)
            {
                string delta = pending.Value.ToString();
                for (int offset = 0; offset < delta.Length;)
                {
                    int count = Math.Min(16384, delta.Length - offset);
                    if (offset + count < delta.Length && char.IsHighSurrogate(delta[offset + count - 1]) && char.IsLowSurrogate(delta[offset + count])) count--;
                    Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(new
                    {
                        protocolVersion = 1, boundary = "host-terminal", @event = "host_terminal_output",
                        stream = pending.Key, delta = delta.Substring(offset, count), sequence = ++_sequence
                    }, Program.JsonOptions));
                    offset += count;
                }
            }
            _pendingEvents.Clear(); _pendingCharacters = 0; _lastPublishedMs = _eventClock.ElapsedMilliseconds;
        }
    }

    internal static string DecodeOutput(byte[] bytes, bool cmdOutput, int? oemCodePage = null)
    {
        if (!cmdOutput) return Encoding.UTF8.GetString(bytes);
        Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
        Encoding oem = Encoding.GetEncoding(oemCodePage ?? (int)GetOEMCP());
        var utf8 = new UTF8Encoding(false, true);
        var result = new StringBuilder();
        int start = 0;
        for (int end = 0; end <= bytes.Length; end++)
        {
            if (end != bytes.Length && bytes[end] != (byte)'\n') continue;
            int count = end - start + (end < bytes.Length ? 1 : 0);
            try { result.Append(utf8.GetString(bytes, start, count)); }
            catch (DecoderFallbackException) { result.Append(oem.GetString(bytes, start, count)); }
            start = end + 1;
        }
        // Arbitrary encodings mixed within one line, or OEM bytes also valid as UTF-8, are inherently ambiguous.
        // The receipt names this per-line policy rather than claiming lossless decoding of every binary stream.
        return result.ToString();
    }

    [DllImport("kernel32.dll")]
    private static extern uint GetOEMCP();
}
