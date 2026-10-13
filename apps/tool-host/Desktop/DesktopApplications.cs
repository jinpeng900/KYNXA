using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32;

namespace KYNXA.ToolHost;

/// <summary>
/// Discovers desktop executables and launches approved applications independently of the helper job.
/// 发现桌面可执行文件，并将已批准的应用以独立于助手作业的生命周期启动。
/// </summary>
internal static class DesktopApplications
{
    private static readonly HashSet<string> BlockedExecutableNames = new(StringComparer.OrdinalIgnoreCase)
    {
        "cmd", "powershell", "powershell_ise", "pwsh", "wscript", "cscript", "mshta", "rundll32", "regsvr32",
        "node", "nodejs", "python", "py", "pypy", "pypy3", "dotnet", "bash", "sh", "wsl",
        "wt", "windowsterminal", "openconsole", "conhost", "java", "msiexec", "installutil"
    };
    private static readonly string[] BlockedLaunchArguments =
    [
        "-e", "--eval", "--execute", "--command", "-command", "-encodedcommand",
        "-enc", "--renderer-cmd-prefix", "--utility-cmd-prefix", "--load-extension", "--no-sandbox"
    ];

    /// <summary>
    /// Returns bounded executable candidates from known system apps and registry paths; it starts no application.
    /// 有界返回已知系统应用及注册表路径中的候选可执行文件，不在发现阶段启动应用。
    /// </summary>
    internal static Dictionary<string, object?> Apps()
    {
        var paths = new Dictionary<string, object>(StringComparer.OrdinalIgnoreCase);
        void Add(string? candidate, string? displayName)
        {
            if (paths.Count >= 128 || string.IsNullOrWhiteSpace(candidate))
            {
                return;
            }
            string path = Environment.ExpandEnvironmentVariables(candidate.Trim().Trim('"'));
            if (!Path.IsPathFullyQualified(path) || !File.Exists(path) || IsBlockedExecutable(path))
            {
                return;
            }
            try
            {
                if (!IsApplicationExecutable(path))
                {
                    return;
                }
                paths.TryAdd(Path.GetFullPath(path), new
                {
                    name = displayName ?? Path.GetFileNameWithoutExtension(path),
                    appPath = Path.GetFullPath(path)
                });
            }
            // An unreadable discovery candidate is omitted; launching remains a separately validated action.
            // 无法读取的发现候选可跳过；实际启动仍是单独校验的操作。
            catch (IOException)
            {
            }
            catch (UnauthorizedAccessException)
            {
            }
        }

        string system = Environment.GetFolderPath(Environment.SpecialFolder.System);
        foreach (string file in new[] { "notepad.exe", "mspaint.exe", "calc.exe" })
        {
            Add(Path.Combine(system, file), null);
        }
        Add(Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Windows), "explorer.exe"), "Explorer");
        foreach (RegistryHive hive in new[] { RegistryHive.CurrentUser, RegistryHive.LocalMachine })
        {
            foreach (RegistryView view in new[] { RegistryView.Registry64, RegistryView.Registry32 })
            {
                try
                {
                    using RegistryKey root = RegistryKey.OpenBaseKey(hive, view);
                    using RegistryKey? key = root.OpenSubKey(@"Software\Microsoft\Windows\CurrentVersion\App Paths", writable: false);
                    if (key is null)
                    {
                        continue;
                    }
                    foreach (string name in key.GetSubKeyNames().Take(256))
                    {
                        using RegistryKey? application = key.OpenSubKey(name, writable: false);
                        if (application?.GetValue(null, null, RegistryValueOptions.DoNotExpandEnvironmentNames) is string path)
                        {
                            Add(path, Path.GetFileNameWithoutExtension(name));
                        }
                    }
                }
                catch (System.Security.SecurityException)
                {
                }
                catch (UnauthorizedAccessException)
                {
                }
            }
        }
        return new()
        {
            ["apps"] = paths.Values.ToArray(),
            ["source"] = "known-apps-and-registry-app-paths",
            ["limit"] = 128
        };
    }

    /// <summary>
    /// Validates the executable and arguments, then returns only the observed launch and placement facts.
    /// 校验程序与参数后启动，回执仅报告实际观察到的启动和窗口放置事实。
    /// </summary>
    internal static Dictionary<string, object?> Launch(DesktopRequest request, CancellationToken cancellation)
    {
        string path = request.AppPath ?? "";
        if (!Path.IsPathFullyQualified(path) || path.StartsWith(@"\\", StringComparison.Ordinal) || !File.Exists(path)
            || IsBlockedExecutable(path) || !IsApplicationExecutable(path))
        {
            throw new DesktopException("DESKTOP_LAUNCH_BLOCKED", "Launch requires an existing absolute application executable; command interpreters and terminal launchers use the host terminal.");
        }
        if (request.Args is null || request.Args.Length > 64 || request.Args.Any(argument =>
            argument is null || argument.Length > 2048
            || argument.Contains('\0')
            || BlockedLaunchArguments.Any(blocked =>
                argument.Equals(blocked, StringComparison.OrdinalIgnoreCase)
                || argument.StartsWith(blocked + "=", StringComparison.OrdinalIgnoreCase))
            || argument.StartsWith("javascript:", StringComparison.OrdinalIgnoreCase)
            || argument.StartsWith("vbscript:", StringComparison.OrdinalIgnoreCase)))
        {
            throw new DesktopException("DESKTOP_LAUNCH_BLOCKED", "Launch arguments contain an unsupported script or command execution mode.");
        }
        ValidateWindowedRuntimeEntry(path, request.Args);
        string executable = Path.GetFullPath(path);
        var commandLine = new StringBuilder(string.Join(" ", new[] { executable }.Concat(request.Args).Select(QuoteArgument)));
        if (commandLine.Length >= 32767)
        {
            throw new DesktopException("DESKTOP_LAUNCH_BLOCKED", "Launch arguments exceed the Windows command-line limit.");
        }
        var startupInformation = new NativeMethods.StartupInfoEx
        {
            StartupInfo = new NativeMethods.StartupInfo { Cb = Marshal.SizeOf<NativeMethods.StartupInfo>() }
        };
        var placement = new DesktopForegroundPlacement();
        if (request.Background)
        {
            // STARTF_USESHOWWINDOW and SW_SHOWNOACTIVATE are display hints, not a focus-policy bypass.
            // STARTF_USESHOWWINDOW 与 SW_SHOWNOACTIVATE 仅提供显示提示，不绕过焦点策略。
            startupInformation.StartupInfo.Flags = 1;
            startupInformation.StartupInfo.ShowWindow = 4;
        }
        var process = new NativeMethods.ProcessInformation();
        try
        {
            // Launched applications must never inherit this helper's gateway IPC pipes.
            // Explicitly leave the launch-only helper job; a denied breakaway must fail, never return a short-lived GUI as success.
            // 已启动应用不得继承网关 IPC 管道；显式脱离仅供启动的助手作业，脱离被拒绝时直接失败，不虚报随后被清理的 GUI 已成功启动。
            if (!NativeMethods.CreateProcessW(
                executable,
                commandLine,
                IntPtr.Zero,
                IntPtr.Zero,
                false,
                NativeMethods.CreateNoWindow | NativeMethods.CreateBreakawayFromJob,
                IntPtr.Zero,
                Path.GetDirectoryName(executable)!,
                ref startupInformation,
                out process))
            {
                throw new DesktopException("DESKTOP_LAUNCH_FAILED", new Win32Exception(Marshal.GetLastWin32Error()).Message);
            }
            if (request.Background)
            {
                placement.ObserveLaunch(checked((int)process.ProcessId), cancellation);
            }
            // The executable may delegate to an existing app. Never attribute another process's window to this launch.
            // 可执行程序可能把请求转交给已有应用，不能把其他进程的窗口归属到本次启动。
            nint foregroundAfter = DesktopNativeMethods.GetForegroundWindow();
            DesktopNativeMethods.GetWindowThreadProcessId(foregroundAfter, out uint foregroundProcessId);
            return new()
            {
                ["processId"] = checked((int)process.ProcessId),
                ["appPath"] = executable,
                ["args"] = request.Args,
                ["windowDiscoveryRequired"] = true,
                ["backgroundRequested"] = request.Background,
                ["backgroundMode"] = request.Background ? "best-effort-no-activate" : "normal",
                ["backgroundWindowObserved"] = placement.WindowObserved,
                ["backgroundPlacementConfirmed"] = placement.WindowPlacedBehind,
                ["focusRestoreAttempted"] = placement.FocusRestoreAttempted,
                ["focusRestoreSucceeded"] = placement.FocusRestoreSucceeded,
                ["isForeground"] = foregroundProcessId == process.ProcessId,
                ["foregroundPreserved"] = placement.ForegroundPreserved,
                ["foregroundObservedAt"] = "launch-return"
            };
        }
        finally
        {
            NativeMethods.Close(ref process.Thread);
            NativeMethods.Close(ref process.Process);
        }
    }

    // Match the Windows command-line parser, including backslashes before quotes and the closing delimiter.
    // 与 Windows 命令行解析规则保持一致，处理引号前与结尾反斜杠，不能简单替换或拼接参数。
    private static string QuoteArgument(string argument)
    {
        var result = new StringBuilder("\"");
        int backslashCount = 0;
        foreach (char character in argument)
        {
            if (character == '\\')
            {
                backslashCount++;
                continue;
            }
            result.Append('\\', character == '"' ? backslashCount * 2 + 1 : backslashCount);
            result.Append(character);
            backslashCount = 0;
        }
        return result.Append('\\', backslashCount * 2).Append('"').ToString();
    }

    private static bool IsBlockedExecutable(string path)
    {
        if (!string.Equals(Path.GetExtension(path), ".exe", StringComparison.OrdinalIgnoreCase))
        {
            return true;
        }
        string name = Path.GetFileNameWithoutExtension(path);
        if (BlockedExecutableNames.Contains(name) || IsBlockedPythonRuntime(name))
        {
            return true;
        }
        try
        {
            string? original = FileVersionInfo.GetVersionInfo(path).OriginalFilename;
            return original is not null && (BlockedExecutableNames.Contains(Path.GetFileNameWithoutExtension(original))
                || IsBlockedPythonRuntime(Path.GetFileNameWithoutExtension(original)));
        }
        catch (IOException)
        {
            return true;
        }
    }

    private static bool IsBlockedPythonRuntime(string name) => name.StartsWith("python", StringComparison.OrdinalIgnoreCase)
        && !name.Equals("pythonw", StringComparison.OrdinalIgnoreCase);

    private static void ValidateWindowedRuntimeEntry(string path, string[] arguments)
    {
        string name = Path.GetFileNameWithoutExtension(path);
        string? original = FileVersionInfo.GetVersionInfo(path).OriginalFilename;
        string identity = original is null ? name : Path.GetFileNameWithoutExtension(original);
        bool pythonWindowed = name.Equals("pythonw", StringComparison.OrdinalIgnoreCase) || identity.Equals("pythonw", StringComparison.OrdinalIgnoreCase);
        bool javaWindowed = name.Equals("javaw", StringComparison.OrdinalIgnoreCase) || identity.Equals("javaw", StringComparison.OrdinalIgnoreCase);
        if (!pythonWindowed && !javaWindowed)
        {
            return;
        }
        string? entry = pythonWindowed && arguments.Length >= 1 ? arguments[0]
            : javaWindowed && arguments.Length >= 2 && arguments[0] == "-jar" ? arguments[1] : null;
        // Windowed runtimes can launch an explicit file entry, never an inline command or implicit module.
        // 窗口运行时只接受明确的文件入口，不允许内联命令或隐式模块执行。
        if (entry is null || !Path.IsPathFullyQualified(entry) || entry.StartsWith(@"\\", StringComparison.Ordinal) || !File.Exists(entry)
            || (pythonWindowed && Path.GetExtension(entry).ToLowerInvariant() is not (".py" or ".pyw"))
            || (javaWindowed && !Path.GetExtension(entry).Equals(".jar", StringComparison.OrdinalIgnoreCase)))
        {
            throw new DesktopException("DESKTOP_LAUNCH_BLOCKED", "A windowed runtime requires an existing absolute .py/.pyw or -jar .jar application entry; use the host terminal for commands.");
        }
    }

    private static bool IsApplicationExecutable(string path)
    {
        // Some desktop apps (for example Blender) use the console subsystem despite presenting a GUI.
        // 部分桌面应用（例如 Blender）使用控制台子系统，不能仅凭 PE 子系统就拒绝合法应用。
        using FileStream stream = new(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
        using var reader = new BinaryReader(stream);
        if (stream.Length < 96 || reader.ReadUInt16() != 0x5a4d)
        {
            return false;
        }
        stream.Position = 0x3c;
        int headerOffset = reader.ReadInt32();
        if (headerOffset < 0 || headerOffset > stream.Length - 94)
        {
            return false;
        }
        stream.Position = headerOffset;
        if (reader.ReadUInt32() != 0x00004550)
        {
            return false;
        }
        stream.Position = headerOffset + 24;
        ushort optionalHeaderMagic = reader.ReadUInt16();
        if (optionalHeaderMagic is not 0x10b and not 0x20b)
        {
            return false;
        }
        stream.Position = headerOffset + 24 + 68;
        return reader.ReadUInt16() is 2 or 3;
    }
}
