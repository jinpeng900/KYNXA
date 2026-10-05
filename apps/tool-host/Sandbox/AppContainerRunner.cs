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
        string workspaceRoot = Path.GetFullPath(request.WorkspaceRoot!);
        string trustedRuntimePath = Path.GetFullPath(request.NodeExecutable!);
        string profileName = "kynxa.run." + Guid.NewGuid().ToString("N");
        string runDirectory = Path.Combine(Path.GetTempPath(), "kynxa-tool-sandbox", Guid.NewGuid().ToString("N"));
        string stagingDirectory = Path.Combine(runDirectory, "workspace");
        IntPtr appContainerSid = IntPtr.Zero, jobHandle = IntPtr.Zero;
        bool profileCreated = false, keepWorkspace = false;
        var processInformation = new ProcessInformation();
        try
        {
            Directory.CreateDirectory(stagingDirectory);
            var snapshot = new WorkspaceSnapshot(request.ExcludedRoots, request.TrustedManagedWorkspace, request.ConversationWorkspaceHome);
            snapshot.Copy(workspaceRoot, stagingDirectory);
            string skillDirectory = Path.Combine(stagingDirectory, ".sandbox-skill");
            if (request.Skill is not null) SkillSnapshot.Copy(request.Skill, skillDirectory);
            string runtimeDirectory = Path.Combine(stagingDirectory, ".sandbox-runtime");
            string temporaryDirectory = Path.Combine(stagingDirectory, ".sandbox-temp");
            Directory.CreateDirectory(runtimeDirectory);
            Directory.CreateDirectory(temporaryDirectory);
            string nodeExecutable = Path.Combine(runtimeDirectory, "node.exe");
            File.Copy(trustedRuntimePath, nodeExecutable);
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

            int profileResult = CreateAppContainerProfile(profileName, "KYNXA terminal", "Isolated per-run terminal", IntPtr.Zero, 0, out appContainerSid);
            if (profileResult < 0) throw new SandboxException("SANDBOX_START_FAILED", $"CreateAppContainerProfile failed (0x{profileResult:X8}).");
            profileCreated = true;
            SetWorkspaceSecurity(stagingDirectory, appContainerSid);
            if (request.Skill is not null) SetReadOnlySkillSecurity(skillDirectory, appContainerSid);
            jobHandle = CreateJobObjectW(IntPtr.Zero, null);
            Check(jobHandle != IntPtr.Zero, "CreateJobObject");
            var limits = new JobExtendedLimitInformation
            {
                BasicLimitInformation = new JobBasicLimitInformation
                {
                    // Children inherit this job. No breakaway flags are enabled.
                    // 子进程继承此作业对象，不启用脱离作业的标志。
                    LimitFlags = 0x2000 | 0x8 | 0x100 | 0x200 | 0x400,
                    ActiveProcessLimit = MaximumProcessCount
                },
                ProcessMemoryLimit = (UIntPtr)MaximumProcessMemoryBytes,
                JobMemoryLimit = (UIntPtr)MaximumJobMemoryBytes
            };
            Check(SetInformationJobObject(jobHandle, 9, ref limits, (uint)Marshal.SizeOf<JobExtendedLimitInformation>()), "SetInformationJobObject");

            var output = new BoundedOutput(jobHandle, isCmd);
            // Node resolves module paths by probing every host ancestor unless symlink preservation is enabled.
            // Preserve paths inside the already link-free snapshot instead of granting access to host ancestors.
            // 未保留符号链接路径时，Node 会探测宿主的各级祖先目录；在已验证无链接的快照内保留路径，避免授权宿主祖先。
            string[] commandArguments = request.Skill is null ? request.Args : [Path.Combine(skillDirectory, request.Skill.Script.Replace('/', Path.DirectorySeparatorChar)), .. request.Args];
            string[] effectiveArguments = isCmd ? commandArguments : ["--preserve-symlinks", "--preserve-symlinks-main", .. commandArguments];
            var (stdout, stderr) = StartSandboxProcess(executable, effectiveArguments, stagingDirectory, temporaryDirectory, appContainerSid, jobHandle, ref processInformation, output, isCmd);
            Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(new
            {
                protocolVersion = 1, @event = "sandbox_started", processId = processInformation.ProcessId, stagingDirectory = stagingDirectory
            }, Program.JsonOptions));
            bool timedOut = false, cancelled = false;
            var elapsed = System.Diagnostics.Stopwatch.StartNew();
            while (WaitForSingleObject(processInformation.Process, 25) == WaitTimeout)
            {
                if (cancellationToken.IsCancellationRequested)
                {
                    cancelled = true;
                    Check(TerminateJobObject(jobHandle, 125), "TerminateJobObject cancellation");
                    break;
                }
                if (elapsed.ElapsedMilliseconds >= request.TimeoutMs)
                {
                    timedOut = true;
                    Check(TerminateJobObject(jobHandle, 124), "TerminateJobObject timeout");
                    break;
                }
                await Task.Delay(10);
            }
            // Also kill descendants when the original script exits normally.
            // 原脚本正常退出时也终止仍存活的子孙进程。
            Check(TerminateJobObject(jobHandle, timedOut ? 124u : 125u), "TerminateJobObject completion");
            await WaitForEmptyJobAsync(jobHandle);
            Check(GetExitCodeProcess(processInformation.Process, out uint exitCode), "GetExitCodeProcess");
            await Task.WhenAll(stdout, stderr);
            keepWorkspace = !cancelled;
            return new
            {
                protocolVersion = 1, exitCode = unchecked((int)exitCode), stdout = await stdout, stderr = await stderr,
                timedOut, cancelled, sandbox = "appcontainer", workspaceCopy = true, stagingDirectory = stagingDirectory,
                processId = processInformation.ProcessId, activeProcessesAfterExit = 0,
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
            // 作业句柄不继承且只有一个；宿主终止时关闭它也会结束整个进程树。
            if (jobHandle != IntPtr.Zero) TerminateJobObject(jobHandle, 125);
            Close(ref processInformation.Thread);
            Close(ref processInformation.Process);
            Close(ref jobHandle);
            if (appContainerSid != IntPtr.Zero) FreeSid(appContainerSid);
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

    private static void SetWorkspaceSecurity(string stagingDirectory, IntPtr appContainerSid)
    {
        Check(ConvertSidToStringSidW(appContainerSid, out IntPtr sidText), "ConvertSidToStringSid");
        try
        {
            string appSid = Marshal.PtrToStringUni(sidText)!;
            string userSid = WindowsIdentity.GetCurrent().User?.Value
                ?? throw new SandboxException("SANDBOX_START_FAILED", "Cannot identify the current Windows user.");
            // A protected DACL excludes other app containers; low integrity enables writes only here.
            // 受保护 DACL 排除其他 AppContainer，低完整性标签只允许在此处写入。
            string descriptor = $"D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{userSid})(A;OICI;0x1301bf;;;{appSid})S:(ML;OICI;NW;;;LW)";
            Check(ConvertStringSecurityDescriptorToSecurityDescriptorW(descriptor, 1, out IntPtr security, out _), "ConvertSecurityDescriptor");
            try
            {
                Check(SetFileSecurityW(stagingDirectory, 0x80000004 | 0x10, security), "SetFileSecurity sandbox workspace");
                // Existing snapshot descendants must receive the same ACL and integrity label.
                // 已有快照的所有子项必须使用相同 ACL 与完整性标签。
                foreach (string path in Directory.EnumerateFileSystemEntries(stagingDirectory, "*", SearchOption.AllDirectories))
                    Check(SetFileSecurityW(path, 0x80000004 | 0x10, security), "SetFileSecurity sandbox content");
            }
            finally { LocalFree(security); }
        }
        finally { LocalFree(sidText); }
    }

    private static void SetReadOnlySkillSecurity(string folder, IntPtr appContainerSid)
    {
        Check(ConvertSidToStringSidW(appContainerSid, out IntPtr sidText), "ConvertSidToStringSid skill");
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

    private static (Task<string> Stdout, Task<string> Stderr) StartSandboxProcess(string executable, string[] commandArguments, string stagingDirectory,
        string temporaryDirectory, IntPtr appContainerSid, IntPtr jobHandle, ref ProcessInformation processInformation, BoundedOutput output, bool isCmd)
    {
        IntPtr stdoutRead = IntPtr.Zero, stdoutWrite = IntPtr.Zero, stderrRead = IntPtr.Zero, stderrWrite = IntPtr.Zero;
        IntPtr stdinRead = IntPtr.Zero, stdinWrite = IntPtr.Zero, attributeList = IntPtr.Zero;
        IntPtr capabilitiesBuffer = IntPtr.Zero, handlesBuffer = IntPtr.Zero, environmentBlock = IntPtr.Zero;
        try
        {
            var security = new SecurityAttributes { Length = Marshal.SizeOf<SecurityAttributes>(), InheritHandle = 1 };
            Check(CreatePipe(out stdoutRead, out stdoutWrite, ref security, 0), "CreatePipe stdout");
            Check(CreatePipe(out stderrRead, out stderrWrite, ref security, 0), "CreatePipe stderr");
            Check(CreatePipe(out stdinRead, out stdinWrite, ref security, 0), "CreatePipe stdin");
            Check(SetHandleInformation(stdoutRead, 1, 0), "Protect stdout reader");
            Check(SetHandleInformation(stderrRead, 1, 0), "Protect stderr reader");
            Check(SetHandleInformation(stdinWrite, 1, 0), "Protect stdin writer");
            UIntPtr attributeListBytes = UIntPtr.Zero;
            InitializeProcThreadAttributeList(IntPtr.Zero, 2, 0, ref attributeListBytes);
            attributeList = Marshal.AllocHGlobal(checked((int)attributeListBytes.ToUInt64()));
            Check(InitializeProcThreadAttributeList(attributeList, 2, 0, ref attributeListBytes), "InitializeProcThreadAttributeList");
            var capabilities = new SecurityCapabilities { AppContainerSid = appContainerSid };
            capabilitiesBuffer = Marshal.AllocHGlobal(Marshal.SizeOf<SecurityCapabilities>());
            Marshal.StructureToPtr(capabilities, capabilitiesBuffer, false);
            Check(UpdateProcThreadAttribute(attributeList, 0, (IntPtr)0x20009, capabilitiesBuffer,
                (UIntPtr)Marshal.SizeOf<SecurityCapabilities>(), IntPtr.Zero, IntPtr.Zero), "Set AppContainer security capabilities");
            handlesBuffer = Marshal.AllocHGlobal(IntPtr.Size * 3);
            Marshal.WriteIntPtr(handlesBuffer, stdoutWrite);
            Marshal.WriteIntPtr(handlesBuffer, IntPtr.Size, stderrWrite);
            Marshal.WriteIntPtr(handlesBuffer, IntPtr.Size * 2, stdinRead);
            Check(UpdateProcThreadAttribute(attributeList, 0, (IntPtr)0x20002, handlesBuffer,
                (UIntPtr)(IntPtr.Size * 3), IntPtr.Zero, IntPtr.Zero), "Set inherited handle allowlist");
            var startup = new StartupInfoEx
            {
                AttributeList = attributeList,
                StartupInfo = new StartupInfo
                {
                    Cb = Marshal.SizeOf<StartupInfoEx>(), Flags = StartfUseStdHandles,
                    StdInput = stdinRead, StdOutput = stdoutWrite, StdError = stderrWrite
                }
            };
            environmentBlock = Marshal.StringToHGlobalUni(BuildEnvironment(stagingDirectory, temporaryDirectory));
            string arguments = isCmd
                ? QuoteArgument(executable) + " /d /u /s /c \"" + commandArguments[2] + "\""
                : string.Join(" ", new[] { executable }.Concat(commandArguments).Select(QuoteArgument));
            var commandLine = new StringBuilder(arguments);
            Check(CreateProcessW(executable, commandLine, IntPtr.Zero, IntPtr.Zero, true,
                CreateSuspended | CreateUnicodeEnvironment | ExtendedStartupInfoPresent | CreateNoWindow,
                environmentBlock, stagingDirectory, ref startup, out processInformation), "CreateProcess AppContainer");
            try
            {
                Check(AssignProcessToJobObject(jobHandle, processInformation.Process), "AssignProcessToJobObject");
                Check(OpenProcessToken(processInformation.Process, TokenQuery, out IntPtr processTokenHandle), "Open child process token");
                try
                {
                    Check(GetTokenInformation(processTokenHandle, TokenIsAppContainer, out uint isAppContainer, 4, out _), "Check child AppContainer token");
                    if (isAppContainer != 1) throw new SandboxException("SANDBOX_START_FAILED", "Windows did not create an AppContainer token.");
                }
                finally { Close(ref processTokenHandle); }
                if (ResumeThread(processInformation.Thread) == uint.MaxValue) Check(false, "ResumeThread");
            }
            catch { TerminateProcess(processInformation.Process, 125); throw; }
            Close(ref stdoutWrite);
            Close(ref stderrWrite);
            Close(ref stdinRead);
            Close(ref stdinWrite); // The script receives EOF; no interactive host input is exposed. 中文：脚本收到 EOF，不暴露宿主交互输入。
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
            if (attributeList != IntPtr.Zero) { DeleteProcThreadAttributeList(attributeList); Marshal.FreeHGlobal(attributeList); }
            if (capabilitiesBuffer != IntPtr.Zero) Marshal.FreeHGlobal(capabilitiesBuffer);
            if (handlesBuffer != IntPtr.Zero) Marshal.FreeHGlobal(handlesBuffer);
            if (environmentBlock != IntPtr.Zero) Marshal.FreeHGlobal(environmentBlock);
        }
    }

    private static string BuildEnvironment(string stagingDirectory, string temporaryDirectory)
    {
        string windows = Environment.GetFolderPath(Environment.SpecialFolder.Windows);
        var values = new SortedDictionary<string, string>(StringComparer.OrdinalIgnoreCase)
        {
            ["SYSTEMROOT"] = windows, ["WINDIR"] = windows,
            ["TEMP"] = temporaryDirectory, ["TMP"] = temporaryDirectory,
            ["USERPROFILE"] = temporaryDirectory, ["HOME"] = temporaryDirectory,
            ["APPDATA"] = temporaryDirectory, ["LOCALAPPDATA"] = temporaryDirectory,
            ["COMSPEC"] = Path.Combine(stagingDirectory, ".sandbox-runtime", "cmd.exe"),
            ["PATH"] = Path.Combine(stagingDirectory, ".sandbox-runtime"), ["KYNXA_SANDBOX"] = "appcontainer"
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

    private static async Task WaitForEmptyJobAsync(IntPtr jobHandle)
    {
        for (int attempt = 0; attempt < 200; attempt++)
        {
            Check(QueryInformationJobObject(jobHandle, 1, out var information, (uint)Marshal.SizeOf<JobBasicAccountingInformation>(), IntPtr.Zero), "QueryInformationJobObject");
            if (information.ActiveProcesses == 0) return;
            await Task.Delay(10);
        }
        throw new SandboxException("SANDBOX_CLEANUP_FAILED", "The job still reports live processes after termination.");
    }

    private sealed class BoundedOutput(IntPtr jobHandle, bool cmdOutput)
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
                    TerminateJobObject(jobHandle, 126);
            }
            byte[] bytes = stored.ToArray();
            // /u produces UTF-16 for cmd builtins. Child programs and TYPE may emit plain UTF-8 bytes.
            // /u 使 cmd 内置命令输出 UTF-16，而子程序或 TYPE 仍可能输出普通 UTF-8 字节。
            return cmdOutput && bytes.Contains((byte)0) ? Encoding.Unicode.GetString(bytes) : Encoding.UTF8.GetString(bytes);
        });
    }
}
