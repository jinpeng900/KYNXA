using Microsoft.Win32.SafeHandles;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text;
using static KYNXA.ToolHost.NativeMethods;

namespace KYNXA.ToolHost;

internal static class AppContainerRunner
{
    private const int MaximumOutputBytes = 256 * 1024;
    private const int MaximumProcessCount = 16;
    private const long MaximumProcessMemoryBytes = 256L * 1024 * 1024;
    private const long MaximumJobMemoryBytes = 768L * 1024 * 1024;

    internal static async Task<object> RunAsync(SandboxRequest request, CancellationToken cancellationToken)
    {
        Validate(request);
        string workspace = Path.GetFullPath(request.WorkspaceRoot!);
        string runtime = Path.GetFullPath(request.NodeExecutable!);
        string profileName = "kynxa.run." + Guid.NewGuid().ToString("N");
        string runDirectory = Path.Combine(Path.GetTempPath(), "kynxa-tool-sandbox", Guid.NewGuid().ToString("N"));
        string stage = Path.Combine(runDirectory, "workspace");
        IntPtr sid = IntPtr.Zero, job = IntPtr.Zero;
        bool profileCreated = false, keepWorkspace = false;
        var process = new ProcessInformation();
        try
        {
            Directory.CreateDirectory(stage);
            var snapshot = new WorkspaceSnapshot(request.ExcludedRoots, request.TrustedManagedWorkspace);
            snapshot.Copy(workspace, stage);
            string skillDirectory = Path.Combine(stage, ".sandbox-skill");
            if (request.Skill is not null) SkillSnapshot.Copy(request.Skill, skillDirectory);
            string runtimeDirectory = Path.Combine(stage, ".sandbox-runtime");
            string temporaryDirectory = Path.Combine(stage, ".sandbox-temp");
            Directory.CreateDirectory(runtimeDirectory);
            Directory.CreateDirectory(temporaryDirectory);
            string nodeExecutable = Path.Combine(runtimeDirectory, "node.exe");
            File.Copy(runtime, nodeExecutable);
            bool isCmd = request.Command is "cmd" or "cmd.exe";
            string executable = nodeExecutable;
            if (isCmd)
            {
                string systemCmd = Path.Combine(Environment.SystemDirectory, "cmd.exe");
                if (!File.Exists(systemCmd) || (File.GetAttributes(systemCmd) & FileAttributes.ReparsePoint) != 0)
                    throw new SandboxException("SANDBOX_COMMAND_UNSUPPORTED", "The pinned Windows command processor is unavailable.");
                executable = Path.Combine(runtimeDirectory, "cmd.exe");
                File.Copy(systemCmd, executable);
            }
            cancellationToken.ThrowIfCancellationRequested();

            int hr = CreateAppContainerProfile(profileName, "KYNXA terminal", "Isolated per-run terminal", IntPtr.Zero, 0, out sid);
            if (hr < 0) throw new SandboxException("SANDBOX_START_FAILED", $"CreateAppContainerProfile failed (0x{hr:X8}).");
            profileCreated = true;
            SetWorkspaceSecurity(stage, sid);
            if (request.Skill is not null) SetReadOnlySkillSecurity(skillDirectory, sid);
            job = CreateJobObjectW(IntPtr.Zero, null);
            Check(job != IntPtr.Zero, "CreateJobObject");
            var limits = new JobExtendedLimitInformation
            {
                BasicLimitInformation = new JobBasicLimitInformation
                {
                    // Children inherit this job. No breakaway flags are enabled.
                    LimitFlags = 0x2000 | 0x8 | 0x100 | 0x200 | 0x400,
                    ActiveProcessLimit = MaximumProcessCount
                },
                ProcessMemoryLimit = (UIntPtr)MaximumProcessMemoryBytes,
                JobMemoryLimit = (UIntPtr)MaximumJobMemoryBytes
            };
            Check(SetInformationJobObject(job, 9, ref limits, (uint)Marshal.SizeOf<JobExtendedLimitInformation>()), "SetInformationJobObject");

            var output = new BoundedOutput(job, isCmd);
            // Node resolves module paths by probing every host ancestor unless symlink preservation is enabled.
            // Preserve paths inside the already link-free snapshot instead of granting access to host ancestors.
            string[] commandArgs = request.Skill is null ? request.Args : [Path.Combine(skillDirectory, request.Skill.Script.Replace('/', Path.DirectorySeparatorChar)), .. request.Args];
            string[] effectiveArgs = isCmd ? commandArgs : ["--preserve-symlinks", "--preserve-symlinks-main", .. commandArgs];
            var (stdout, stderr) = Start(executable, effectiveArgs, stage, temporaryDirectory, sid, job, ref process, output, isCmd);
            Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(new
            {
                protocolVersion = 1, @event = "sandbox_started", processId = process.ProcessId, stagingDirectory = stage
            }, Program.JsonOptions));
            bool timedOut = false, cancelled = false;
            var elapsed = System.Diagnostics.Stopwatch.StartNew();
            while (WaitForSingleObject(process.Process, 25) == WaitTimeout)
            {
                if (cancellationToken.IsCancellationRequested)
                {
                    cancelled = true;
                    Check(TerminateJobObject(job, 125), "TerminateJobObject cancellation");
                    break;
                }
                if (elapsed.ElapsedMilliseconds >= request.TimeoutMs)
                {
                    timedOut = true;
                    Check(TerminateJobObject(job, 124), "TerminateJobObject timeout");
                    break;
                }
                await Task.Delay(10);
            }
            // Also kill descendants when the original script exits normally.
            Check(TerminateJobObject(job, timedOut ? 124u : 125u), "TerminateJobObject completion");
            await WaitForEmptyJobAsync(job);
            Check(GetExitCodeProcess(process.Process, out uint exitCode), "GetExitCodeProcess");
            await Task.WhenAll(stdout, stderr);
            keepWorkspace = !cancelled;
            return new
            {
                protocolVersion = 1, exitCode = unchecked((int)exitCode), stdout = await stdout, stderr = await stderr,
                timedOut, cancelled, sandbox = "appcontainer", workspaceCopy = true, stagingDirectory = stage,
                processId = process.ProcessId, activeProcessesAfterExit = 0,
                outputTruncated = output.Truncated, outputLimitExceeded = output.Truncated,
                snapshot = new { files = snapshot.Files, bytes = snapshot.Bytes, skipped = snapshot.Skipped },
                limits = new { timeoutMs = request.TimeoutMs, outputBytes = MaximumOutputBytes,
                    processCount = MaximumProcessCount, processMemoryBytes = MaximumProcessMemoryBytes, jobMemoryBytes = MaximumJobMemoryBytes },
                network = false, tokenVerified = true, elapsedMs = elapsed.ElapsedMilliseconds,
                skillExecution = request.Skill is null ? null : new
                {
                    manifestVersion = 1, readOnlyPackage = true, hashChecked = true,
                    script = request.Skill.Script, fileCount = request.Skill.Files.Length,
                    scriptSha256 = request.Skill.Files.Single(file => file.Path == request.Skill.Script).Sha256
                },
                runtimeArguments = isCmd ? new[] { "/d", "/u", "/s", "/c" } : new[] { "--preserve-symlinks", "--preserve-symlinks-main" }
            };
        }
        finally
        {
            // Closing the sole non-inherited job handle also kills the tree if this host is terminated.
            if (job != IntPtr.Zero) TerminateJobObject(job, 125);
            Close(ref process.Thread);
            Close(ref process.Process);
            Close(ref job);
            if (sid != IntPtr.Zero) FreeSid(sid);
            if (profileCreated) DeleteAppContainerProfile(profileName);
            if (!keepWorkspace) WorkspaceSnapshot.DeleteOwnedRun(runDirectory);
        }
    }

    private static void Validate(SandboxRequest request)
    {
        if (!OperatingSystem.IsWindowsVersionAtLeast(6, 2)) throw new SandboxException("SANDBOX_UNAVAILABLE", "AppContainer requires Windows 8 or later.");
        if (request.Command is not ("node" or "node.exe" or "cmd" or "cmd.exe"))
            throw new SandboxException("SANDBOX_COMMAND_UNSUPPORTED", "Only isolated Node and cmd commands are supported.");
        if (request.Skill is not null && request.Command is not ("node" or "node.exe"))
            throw new SandboxException("APP_SKILL_SCRIPT_UNSUPPORTED", "Skill scripts use the verified Node.js runtime.");
        if (string.IsNullOrWhiteSpace(request.WorkspaceRoot) || string.IsNullOrWhiteSpace(request.NodeExecutable))
            throw new SandboxException("SANDBOX_INVALID_REQUEST", "Workspace and trusted Node runtime are required.");
        if (!Path.IsPathFullyQualified(request.WorkspaceRoot) || request.WorkspaceRoot.StartsWith("\\\\", StringComparison.Ordinal) ||
            !Path.IsPathFullyQualified(request.NodeExecutable) || request.NodeExecutable.StartsWith("\\\\", StringComparison.Ordinal))
            throw new SandboxException("SANDBOX_INVALID_REQUEST", "Only local absolute workspace and runtime paths are accepted.");
        if (!File.Exists(request.NodeExecutable) || !Path.GetFileName(request.NodeExecutable).Equals("node.exe", StringComparison.OrdinalIgnoreCase))
            throw new SandboxException("SANDBOX_COMMAND_UNSUPPORTED", "Trusted Node executable is missing.");
        if (new FileInfo(request.NodeExecutable).Length > 160 * 1024 * 1024 ||
            (File.GetAttributes(request.NodeExecutable) & FileAttributes.ReparsePoint) != 0)
            throw new SandboxException("SANDBOX_COMMAND_UNSUPPORTED", "Trusted Node executable cannot be linked or exceed the runtime copy limit.");
        if (request.TimeoutMs is < 100 or > 120000) throw new SandboxException("SANDBOX_INVALID_REQUEST", "Timeout must be between 100 and 120000 milliseconds.");
        if (request.Args.Length > 128 || request.Args.Any(argument => argument is null || argument.Contains('\0')) ||
            request.Args.Sum(argument => argument.Length) > 24000)
            throw new SandboxException("SANDBOX_INVALID_REQUEST", "Command arguments exceed their bounds or contain a NUL character.");
        if (request.Args.Any(argument => argument.StartsWith("--inspect", StringComparison.Ordinal) || argument.StartsWith("--debug", StringComparison.Ordinal)))
            throw new SandboxException("SANDBOX_COMMAND_UNSUPPORTED", "Node debugger endpoints are not enabled in the sandbox.");
        if (request.Command is "cmd" or "cmd.exe")
        {
            if (request.Args.Length != 3 || !request.Args[0].Equals("/d", StringComparison.OrdinalIgnoreCase) ||
                !request.Args[1].Equals("/c", StringComparison.OrdinalIgnoreCase) || string.IsNullOrWhiteSpace(request.Args[2]))
                throw new SandboxException("SANDBOX_INVALID_REQUEST", "cmd accepts exactly ['/d', '/c', 'command text']; interactive input and AutoRun are disabled.");
        }
    }

    private static void SetWorkspaceSecurity(string stage, IntPtr sid)
    {
        Check(ConvertSidToStringSidW(sid, out IntPtr sidText), "ConvertSidToStringSid");
        try
        {
            string appSid = Marshal.PtrToStringUni(sidText)!;
            string userSid = WindowsIdentity.GetCurrent().User?.Value
                ?? throw new SandboxException("SANDBOX_START_FAILED", "Cannot identify the current Windows user.");
            // A protected DACL excludes other app containers; low integrity enables writes only here.
            string descriptor = $"D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{userSid})(A;OICI;0x1301bf;;;{appSid})S:(ML;OICI;NW;;;LW)";
            Check(ConvertStringSecurityDescriptorToSecurityDescriptorW(descriptor, 1, out IntPtr security, out _), "ConvertSecurityDescriptor");
            try
            {
                Check(SetFileSecurityW(stage, 0x80000004 | 0x10, security), "SetFileSecurity sandbox workspace");
                // Existing snapshot descendants must receive the same ACL and integrity label.
                foreach (string path in Directory.EnumerateFileSystemEntries(stage, "*", SearchOption.AllDirectories))
                    Check(SetFileSecurityW(path, 0x80000004 | 0x10, security), "SetFileSecurity sandbox content");
            }
            finally { LocalFree(security); }
        }
        finally { LocalFree(sidText); }
    }

    private static void SetReadOnlySkillSecurity(string folder, IntPtr sid)
    {
        Check(ConvertSidToStringSidW(sid, out IntPtr sidText), "ConvertSidToStringSid skill");
        try
        {
            string appSid = Marshal.PtrToStringUni(sidText)!;
            string userSid = WindowsIdentity.GetCurrent().User!.Value;
            string descriptor = $"D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{userSid})(A;OICI;0x1200a9;;;{appSid})S:(ML;OICI;NW;;;LW)";
            Check(ConvertStringSecurityDescriptorToSecurityDescriptorW(descriptor, 1, out IntPtr security, out _), "ConvertSkillSecurityDescriptor");
            try
            {
                Check(SetFileSecurityW(folder, 0x80000004 | 0x10, security), "SetFileSecurity skill package");
                foreach (string path in Directory.EnumerateFileSystemEntries(folder, "*", SearchOption.AllDirectories))
                    Check(SetFileSecurityW(path, 0x80000004 | 0x10, security), "SetFileSecurity skill resource");
            }
            finally { LocalFree(security); }
        }
        finally { LocalFree(sidText); }
    }

    private static (Task<string> Stdout, Task<string> Stderr) Start(string executable, string[] args, string stage,
        string temporaryDirectory, IntPtr sid, IntPtr job, ref ProcessInformation process, BoundedOutput output, bool isCmd)
    {
        IntPtr stdoutRead = IntPtr.Zero, stdoutWrite = IntPtr.Zero, stderrRead = IntPtr.Zero, stderrWrite = IntPtr.Zero;
        IntPtr stdinRead = IntPtr.Zero, stdinWrite = IntPtr.Zero, attributes = IntPtr.Zero;
        IntPtr capabilitiesBuffer = IntPtr.Zero, handlesBuffer = IntPtr.Zero, environment = IntPtr.Zero;
        try
        {
            var security = new SecurityAttributes { Length = Marshal.SizeOf<SecurityAttributes>(), InheritHandle = 1 };
            Check(CreatePipe(out stdoutRead, out stdoutWrite, ref security, 0), "CreatePipe stdout");
            Check(CreatePipe(out stderrRead, out stderrWrite, ref security, 0), "CreatePipe stderr");
            Check(CreatePipe(out stdinRead, out stdinWrite, ref security, 0), "CreatePipe stdin");
            Check(SetHandleInformation(stdoutRead, 1, 0), "Protect stdout reader");
            Check(SetHandleInformation(stderrRead, 1, 0), "Protect stderr reader");
            Check(SetHandleInformation(stdinWrite, 1, 0), "Protect stdin writer");
            UIntPtr size = UIntPtr.Zero;
            InitializeProcThreadAttributeList(IntPtr.Zero, 2, 0, ref size);
            attributes = Marshal.AllocHGlobal(checked((int)size.ToUInt64()));
            Check(InitializeProcThreadAttributeList(attributes, 2, 0, ref size), "InitializeProcThreadAttributeList");
            var capabilities = new SecurityCapabilities { AppContainerSid = sid };
            capabilitiesBuffer = Marshal.AllocHGlobal(Marshal.SizeOf<SecurityCapabilities>());
            Marshal.StructureToPtr(capabilities, capabilitiesBuffer, false);
            Check(UpdateProcThreadAttribute(attributes, 0, (IntPtr)0x20009, capabilitiesBuffer,
                (UIntPtr)Marshal.SizeOf<SecurityCapabilities>(), IntPtr.Zero, IntPtr.Zero), "Set AppContainer security capabilities");
            handlesBuffer = Marshal.AllocHGlobal(IntPtr.Size * 3);
            Marshal.WriteIntPtr(handlesBuffer, stdoutWrite);
            Marshal.WriteIntPtr(handlesBuffer, IntPtr.Size, stderrWrite);
            Marshal.WriteIntPtr(handlesBuffer, IntPtr.Size * 2, stdinRead);
            Check(UpdateProcThreadAttribute(attributes, 0, (IntPtr)0x20002, handlesBuffer,
                (UIntPtr)(IntPtr.Size * 3), IntPtr.Zero, IntPtr.Zero), "Set inherited handle allowlist");
            var startup = new StartupInfoEx
            {
                AttributeList = attributes,
                StartupInfo = new StartupInfo
                {
                    Cb = Marshal.SizeOf<StartupInfoEx>(), Flags = StartfUseStdHandles,
                    StdInput = stdinRead, StdOutput = stdoutWrite, StdError = stderrWrite
                }
            };
            environment = Marshal.StringToHGlobalUni(BuildEnvironment(stage, temporaryDirectory));
            string arguments = isCmd
                ? QuoteArgument(executable) + " /d /u /s /c \"" + args[2] + "\""
                : string.Join(" ", new[] { executable }.Concat(args).Select(QuoteArgument));
            var commandLine = new StringBuilder(arguments);
            Check(CreateProcessW(executable, commandLine, IntPtr.Zero, IntPtr.Zero, true,
                CreateSuspended | CreateUnicodeEnvironment | ExtendedStartupInfoPresent | CreateNoWindow,
                environment, stage, ref startup, out process), "CreateProcess AppContainer");
            try
            {
                Check(AssignProcessToJobObject(job, process.Process), "AssignProcessToJobObject");
                Check(OpenProcessToken(process.Process, TokenQuery, out IntPtr token), "Open child process token");
                try
                {
                    Check(GetTokenInformation(token, TokenIsAppContainer, out uint isAppContainer, 4, out _), "Check child AppContainer token");
                    if (isAppContainer != 1) throw new SandboxException("SANDBOX_START_FAILED", "Windows did not create an AppContainer token.");
                }
                finally { Close(ref token); }
                if (ResumeThread(process.Thread) == uint.MaxValue) Check(false, "ResumeThread");
            }
            catch { TerminateProcess(process.Process, 125); throw; }
            Close(ref stdoutWrite);
            Close(ref stderrWrite);
            Close(ref stdinRead);
            Close(ref stdinWrite); // The script receives EOF; no interactive host input is exposed.
            var stdoutTask = output.ReadAsync(stdoutRead);
            stdoutRead = IntPtr.Zero;
            var stderrTask = output.ReadAsync(stderrRead);
            stderrRead = IntPtr.Zero;
            return (stdoutTask, stderrTask);
        }
        finally
        {
            Close(ref stdoutRead); Close(ref stdoutWrite); Close(ref stderrRead); Close(ref stderrWrite);
            Close(ref stdinRead); Close(ref stdinWrite);
            if (attributes != IntPtr.Zero) { DeleteProcThreadAttributeList(attributes); Marshal.FreeHGlobal(attributes); }
            if (capabilitiesBuffer != IntPtr.Zero) Marshal.FreeHGlobal(capabilitiesBuffer);
            if (handlesBuffer != IntPtr.Zero) Marshal.FreeHGlobal(handlesBuffer);
            if (environment != IntPtr.Zero) Marshal.FreeHGlobal(environment);
        }
    }

    private static string BuildEnvironment(string stage, string temporaryDirectory)
    {
        string windows = Environment.GetFolderPath(Environment.SpecialFolder.Windows);
        var values = new SortedDictionary<string, string>(StringComparer.OrdinalIgnoreCase)
        {
            ["SYSTEMROOT"] = windows, ["WINDIR"] = windows,
            ["TEMP"] = temporaryDirectory, ["TMP"] = temporaryDirectory,
            ["USERPROFILE"] = temporaryDirectory, ["HOME"] = temporaryDirectory,
            ["APPDATA"] = temporaryDirectory, ["LOCALAPPDATA"] = temporaryDirectory,
            ["COMSPEC"] = Path.Combine(stage, ".sandbox-runtime", "cmd.exe"),
            ["PATH"] = Path.Combine(stage, ".sandbox-runtime"), ["KYNXA_SANDBOX"] = "appcontainer"
        };
        return string.Join('\0', values.Select(pair => pair.Key + "=" + pair.Value)) + "\0\0";
    }

    private static string QuoteArgument(string argument)
    {
        var result = new StringBuilder("\"");
        int slashes = 0;
        foreach (char character in argument)
        {
            if (character == '\\') { slashes++; continue; }
            if (character == '"') result.Append('\\', slashes * 2 + 1);
            else result.Append('\\', slashes);
            result.Append(character);
            slashes = 0;
        }
        result.Append('\\', slashes * 2).Append('"');
        return result.ToString();
    }

    private static async Task WaitForEmptyJobAsync(IntPtr job)
    {
        for (int attempt = 0; attempt < 200; attempt++)
        {
            Check(QueryInformationJobObject(job, 1, out var information, (uint)Marshal.SizeOf<JobBasicAccountingInformation>(), IntPtr.Zero), "QueryInformationJobObject");
            if (information.ActiveProcesses == 0) return;
            await Task.Delay(10);
        }
        throw new SandboxException("SANDBOX_CLEANUP_FAILED", "The job still reports live processes after termination.");
    }

    private sealed class BoundedOutput(IntPtr job, bool cmdOutput)
    {
        private int _bytes, _truncated;
        internal bool Truncated => Volatile.Read(ref _truncated) != 0;

        internal Task<string> ReadAsync(IntPtr handle) => Task.Run(() =>
        {
            using var stream = new FileStream(new SafeFileHandle(handle, ownsHandle: true), FileAccess.Read);
            using var stored = new MemoryStream();
            byte[] buffer = new byte[4096];
            int count;
            while ((count = stream.Read(buffer)) != 0)
            {
                int total = Interlocked.Add(ref _bytes, count);
                int accepted = Math.Clamp(MaximumOutputBytes - (total - count), 0, count);
                stored.Write(buffer, 0, accepted);
                if (total > MaximumOutputBytes && Interlocked.Exchange(ref _truncated, 1) == 0)
                    TerminateJobObject(job, 126);
            }
            byte[] bytes = stored.ToArray();
            // /u produces UTF-16 for cmd builtins. Child programs and TYPE may emit plain UTF-8 bytes.
            return cmdOutput && bytes.Contains((byte)0) ? Encoding.Unicode.GetString(bytes) : Encoding.UTF8.GetString(bytes);
        });
    }
}
