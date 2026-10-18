using Microsoft.Win32.SafeHandles;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using static KYNXA.ToolHost.NativeMethods;

namespace KYNXA.ToolHost;

internal static class AppContainerRunner
{
    private const int MaximumOutputBytes = 256 * 1024;
    private const int MaximumProcessCount = 16;
    private const long MaximumProcessMemoryBytes = 256L * 1024 * 1024;
    internal const long MaximumJobMemoryBytes = 768L * 1024 * 1024;
    private const long MinimumJobMemoryBytes = 64L * 1024 * 1024;
    private const uint JobCpuRateControlFlags = 0x1 | 0x4;

    internal static async Task<object> RunAsync(SandboxRequest request, CancellationToken cancellationToken)
    {
        Validate(request);
        var resources = ResolveResourceLimits(request.ResourceLimits, Environment.ProcessorCount);
        string workspaceRoot = Path.GetFullPath(request.WorkspaceRoot!);
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
            bool isCmd = request.Command is "cmd" or "cmd.exe";
            string trustedExecutable = isCmd ? Path.Combine(Environment.SystemDirectory, "cmd.exe") : PrivateNodeExecutable();
            if (!File.Exists(trustedExecutable) || (File.GetAttributes(trustedExecutable) & FileAttributes.ReparsePoint) != 0)
                throw new SandboxException("SANDBOX_COMMAND_UNSUPPORTED", "The verified isolated runtime is unavailable.");
            string executable = Path.Combine(runtimeDirectory, isCmd ? "cmd.exe" : "node.exe");
            File.Copy(trustedExecutable, executable);
            if (!isCmd) VerifyNodeHash(executable);
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
                ProcessMemoryLimit = (UIntPtr)resources.ProcessMemoryBytes,
                JobMemoryLimit = (UIntPtr)resources.JobMemoryBytes
            };
            Check(SetInformationJobObject(jobHandle, 9, ref limits, (uint)Marshal.SizeOf<JobExtendedLimitInformation>()), "SetInformationJobObject");
            var cpuLimits = new JobCpuRateControlInformation { ControlFlags = JobCpuRateControlFlags, CpuRate = resources.CpuRate };
            Check(SetJobCpuRateControlInformation(jobHandle, 15, ref cpuLimits, (uint)Marshal.SizeOf<JobCpuRateControlInformation>()), "Set sandbox CPU hard cap");
            // Query the actual Job before creating/resuming a child; requested limits alone are not evidence.
            // 创建或恢复子进程前回读真实 Job；请求中的额度本身不代表限制已经生效。
            Check(QueryJobExtendedLimitInformation(jobHandle, 9, out var appliedLimits,
                (uint)Marshal.SizeOf<JobExtendedLimitInformation>(), IntPtr.Zero), "Query sandbox memory limits");
            Check(QueryJobCpuRateControlInformation(jobHandle, 15, out var appliedCpuLimits,
                (uint)Marshal.SizeOf<JobCpuRateControlInformation>(), IntPtr.Zero), "Query sandbox CPU hard cap");
            if (appliedLimits.BasicLimitInformation.LimitFlags != limits.BasicLimitInformation.LimitFlags ||
                appliedLimits.BasicLimitInformation.ActiveProcessLimit != MaximumProcessCount ||
                appliedLimits.ProcessMemoryLimit.ToUInt64() != (ulong)resources.ProcessMemoryBytes ||
                appliedLimits.JobMemoryLimit.ToUInt64() != (ulong)resources.JobMemoryBytes ||
                appliedCpuLimits.ControlFlags != JobCpuRateControlFlags || appliedCpuLimits.CpuRate != resources.CpuRate)
                throw new SandboxException("SANDBOX_RESOURCE_CONTROL_FAILED", "Windows did not apply the requested bounded sandbox resource controls.");

            var output = new BoundedOutput(isCmd);
            string[] commandArguments = request.Skill is null ? request.Args : [Path.Combine(skillDirectory, request.Skill.Script.Replace('/', Path.DirectorySeparatorChar)), .. request.Args];
            // Resolve modules only within the link-free snapshot; no access to host ancestor directories is added.
            // 模块解析限定在已核验无链接快照中，不为 Node 探测宿主祖先目录扩大权限。
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
                    processCount = MaximumProcessCount, processMemoryBytes = resources.ProcessMemoryBytes, jobMemoryBytes = resources.JobMemoryBytes,
                    resourceVersion = 1, cpuThreads = resources.CpuThreads, cpuRate = resources.CpuRate, cpuRateControl = true },
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

    /// <summary>Validate broker-selected resource budgets without expanding the original sandbox ceilings.</summary>
    /// <remarks>校验代理分配的资源预算，不扩大原有沙箱上限；额度不是 AppContainer 权限边界。</remarks>
    internal static (int CpuThreads, uint CpuRate, long ProcessMemoryBytes, long JobMemoryBytes) ResolveResourceLimits(
        SandboxResourceLimits? requested, int logicalProcessors)
    {
        ArgumentOutOfRangeException.ThrowIfLessThan(logicalProcessors, 1);
        requested ??= new SandboxResourceLimits { Version = 1,
            CpuThreads = Math.Min(2, logicalProcessors), JobMemoryBytes = MaximumJobMemoryBytes };
        if (requested.Version != 1 || requested.CpuThreads < 1 || requested.CpuThreads > logicalProcessors ||
            requested.JobMemoryBytes < MinimumJobMemoryBytes || requested.JobMemoryBytes > MaximumJobMemoryBytes)
            throw new SandboxException("SANDBOX_INVALID_REQUEST", "Sandbox resource limits exceed the supported version, local CPU count or memory bounds.");
        uint cpuRate = checked((uint)((long)requested.CpuThreads * 10000 / logicalProcessors));
        if (cpuRate is < 1 or > 10000)
            throw new SandboxException("SANDBOX_INVALID_REQUEST", "Sandbox CPU rate cannot be represented within Windows Job bounds.");
        return (requested.CpuThreads, cpuRate, Math.Min(MaximumProcessMemoryBytes, requested.JobMemoryBytes), requested.JobMemoryBytes);
    }

    private static void Validate(SandboxRequest request)
    {
        if (!OperatingSystem.IsWindowsVersionAtLeast(6, 2)) throw new SandboxException("SANDBOX_UNAVAILABLE", "AppContainer requires Windows 8 or later.");
        if (request.Command is not ("node" or "node.exe" or "cmd" or "cmd.exe"))
            throw new SandboxException("SANDBOX_COMMAND_UNSUPPORTED", "Only private Node and the isolated Windows command processor are supported.");
        if (request.Skill is not null && request.Command is not ("node" or "node.exe"))
            throw new SandboxException("APP_SKILL_SCRIPT_UNSUPPORTED", "JavaScript skill scripts require the verified private Node runtime.");
        if (request.Command is "node" or "node.exe")
        {
            string expectedRuntime = PrivateNodeExecutable();
            if (request.NodeExecutable is null || !Path.GetFullPath(request.NodeExecutable).Equals(expectedRuntime, StringComparison.OrdinalIgnoreCase))
                throw new SandboxException("SANDBOX_COMMAND_UNSUPPORTED", "The runtime must be the application-managed private Node executable.");
            VerifyNodeHash(expectedRuntime);
            if (request.Args.Any(argument => argument.StartsWith("--inspect", StringComparison.Ordinal) || argument.StartsWith("--debug", StringComparison.Ordinal)))
                throw new SandboxException("SANDBOX_COMMAND_UNSUPPORTED", "Debugger ports are not enabled in the sandbox.");
        }
        if (string.IsNullOrWhiteSpace(request.WorkspaceRoot))
            throw new SandboxException("SANDBOX_INVALID_REQUEST", "A workspace is required.");
        if (!Path.IsPathFullyQualified(request.WorkspaceRoot) || request.WorkspaceRoot.StartsWith("\\\\", StringComparison.Ordinal))
            throw new SandboxException("SANDBOX_INVALID_REQUEST", "Only local absolute workspace paths are accepted.");
        if (request.TimeoutMs is < 100 or > 120000) throw new SandboxException("SANDBOX_INVALID_REQUEST", "Timeout must be between 100 and 120000 milliseconds.");
        if (request.Args.Length > 128 || request.Args.Any(argument => argument is null || argument.Contains('\0')) ||
            request.Args.Sum(argument => argument.Length) > 24000)
            throw new SandboxException("SANDBOX_INVALID_REQUEST", "Command arguments exceed their bounds or contain a NUL character.");
        if (request.Command is "cmd" or "cmd.exe")
        {
            if (request.Args.Length != 3 || !request.Args[0].Equals("/d", StringComparison.OrdinalIgnoreCase) ||
                !request.Args[1].Equals("/c", StringComparison.OrdinalIgnoreCase) || string.IsNullOrWhiteSpace(request.Args[2]))
                throw new SandboxException("SANDBOX_INVALID_REQUEST", "cmd accepts exactly ['/d', '/c', 'command text']; interactive input and AutoRun are disabled.");
        }
    }

    /// <summary>The helper resolves its own application runtime; model input cannot select another executable.</summary>
    /// <remarks>宿主按随包位置确定私有运行时，模型参数不能选择其他可执行文件。</remarks>
    internal static string PrivateNodeExecutable() => Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "..", "runtime", "mcp", "node", "node.exe"));

    internal static bool HasPrivateNode()
    {
        try { VerifyNodeHash(PrivateNodeExecutable()); return true; }
        catch (SandboxException) { return false; }
    }

    private static void VerifyNodeHash(string executable)
    {
        string runtime = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "..", "runtime", "mcp"));
        string manifest = Path.Combine(runtime, "manifest.json");
        try
        {
            WorkspaceSnapshot.EnsureNoReparseAncestors(manifest);
            WorkspaceSnapshot.EnsureNoReparseAncestors(executable);
            using var document = JsonDocument.Parse(File.ReadAllText(manifest));
            string expected = document.RootElement.GetProperty("node").GetProperty("sha256").GetString() ?? "";
            using var input = new FileStream(executable, FileMode.Open, FileAccess.Read, FileShare.Read);
            if (input.Length > 160L * 1024 * 1024 || expected.Length != 64 || !expected.All(Uri.IsHexDigit) ||
                !Convert.ToHexString(SHA256.HashData(input)).Equals(expected, StringComparison.OrdinalIgnoreCase))
                throw new SandboxException("NODE_RUNTIME_INTEGRITY_FAILED", "Private Node integrity verification failed.");
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or JsonException or KeyNotFoundException or InvalidOperationException)
        { throw new SandboxException("NODE_RUNTIME_UNAVAILABLE", "The private Node runtime or its pinned manifest is unavailable."); }
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

    private sealed class BoundedOutput(bool cmdOutput)
    {
        private long _bytes;
        private int _truncated;
        internal bool Truncated => Volatile.Read(ref _truncated) != 0;

        internal Task<string> ReadAsync(IntPtr handle) => Task.Run(() =>
        {
            using var stream = new FileStream(new SafeFileHandle(handle, ownsHandle: true), FileAccess.Read);
            using var stored = new MemoryStream();
            byte[] buffer = new byte[4096];
            int count;
            while ((count = stream.Read(buffer)) != 0)
            {
                long total = Interlocked.Add(ref _bytes, count);
                int accepted = (int)Math.Clamp(MaximumOutputBytes - (total - count), 0L, count);
                stored.Write(buffer, 0, accepted);
                // Continue draining after bounded capture fills; verbose logs must not terminate useful work.
                // 有界保留区满后仍持续排空；日志量大不能终止仍在正常推进的任务。
                if (total > MaximumOutputBytes) Interlocked.Exchange(ref _truncated, 1);
            }
            byte[] bytes = stored.ToArray();
            // /u produces UTF-16 for cmd builtins. Child programs and TYPE may emit plain UTF-8 bytes.
            // /u 使 cmd 内置命令输出 UTF-16，而子程序或 TYPE 仍可能输出普通 UTF-8 字节。
            return cmdOutput && bytes.Contains((byte)0) ? Encoding.Unicode.GetString(bytes) : Encoding.UTF8.GetString(bytes);
        });
    }
}
