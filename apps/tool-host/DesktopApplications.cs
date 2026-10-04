using System.Diagnostics;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32;

namespace KYNXA.ToolHost;

internal static class DesktopApplications
{
    private static readonly HashSet<string> BlockedNames = new(StringComparer.OrdinalIgnoreCase)
    {
        "cmd", "powershell", "powershell_ise", "pwsh", "wscript", "cscript", "mshta", "rundll32", "regsvr32",
        "node", "nodejs", "python", "pythonw", "py", "pypy", "pypy3", "dotnet", "bash", "sh", "wsl",
        "wt", "windowsterminal", "openconsole", "conhost", "java", "javaw", "msiexec", "installutil"
    };
    private static readonly string[] BlockedArguments = ["-e", "--eval", "--execute", "--command", "-command", "-encodedcommand",
        "-enc", "--renderer-cmd-prefix", "--utility-cmd-prefix", "--load-extension", "--no-sandbox"];

    internal static Dictionary<string, object?> Apps()
    {
        var paths = new Dictionary<string, object>(StringComparer.OrdinalIgnoreCase);
        void Add(string? candidate, string? displayName)
        {
            if (paths.Count >= 128 || string.IsNullOrWhiteSpace(candidate)) return;
            string path = Environment.ExpandEnvironmentVariables(candidate.Trim().Trim('"'));
            if (!Path.IsPathFullyQualified(path) || !File.Exists(path) || Blocked(path)) return;
            try
            {
                if (!GuiExecutable(path)) return;
                paths.TryAdd(Path.GetFullPath(path), new { name = displayName ?? Path.GetFileNameWithoutExtension(path), appPath = Path.GetFullPath(path) });
            }
            catch (IOException) { }
            catch (UnauthorizedAccessException) { }
        }
        string system = Environment.GetFolderPath(Environment.SpecialFolder.System);
        foreach (string file in new[] { "notepad.exe", "mspaint.exe", "calc.exe" }) Add(Path.Combine(system, file), null);
        Add(Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Windows), "explorer.exe"), "Explorer");
        foreach (RegistryHive hive in new[] { RegistryHive.CurrentUser, RegistryHive.LocalMachine })
            foreach (RegistryView view in new[] { RegistryView.Registry64, RegistryView.Registry32 })
                try
                {
                    using RegistryKey root = RegistryKey.OpenBaseKey(hive, view);
                    using RegistryKey? key = root.OpenSubKey(@"Software\Microsoft\Windows\CurrentVersion\App Paths", writable: false);
                    if (key is null) continue;
                    foreach (string name in key.GetSubKeyNames().Take(256))
                    {
                        using RegistryKey? application = key.OpenSubKey(name, writable: false);
                        if (application?.GetValue(null, null, RegistryValueOptions.DoNotExpandEnvironmentNames) is string path) Add(path, Path.GetFileNameWithoutExtension(name));
                    }
                }
                catch (System.Security.SecurityException) { }
                catch (UnauthorizedAccessException) { }
        return new() { ["apps"] = paths.Values.ToArray(), ["source"] = "known-apps-and-registry-app-paths", ["limit"] = 128 };
    }

    internal static Dictionary<string, object?> Launch(DesktopRequest request, CancellationToken cancellation)
    {
        string path = request.AppPath ?? "";
        if (!Path.IsPathFullyQualified(path) || path.StartsWith(@"\\", StringComparison.Ordinal) || !File.Exists(path)
            || Blocked(path) || !GuiExecutable(path))
            throw new DesktopException("DESKTOP_LAUNCH_BLOCKED", "Launch requires an existing absolute GUI executable; command interpreters and terminal launchers are not allowed.");
        if (request.Args is null || request.Args.Length > 64 || request.Args.Any(argument => argument is null || argument.Length > 2048
            || argument.Contains('\0') || BlockedArguments.Any(blocked => argument.Equals(blocked, StringComparison.OrdinalIgnoreCase)
                || argument.StartsWith(blocked + "=", StringComparison.OrdinalIgnoreCase))
            || argument.StartsWith("javascript:", StringComparison.OrdinalIgnoreCase) || argument.StartsWith("vbscript:", StringComparison.OrdinalIgnoreCase)))
            throw new DesktopException("DESKTOP_LAUNCH_BLOCKED", "Launch arguments contain an unsupported script or command execution mode.");
        string executable = Path.GetFullPath(path);
        var commandLine = new StringBuilder(string.Join(" ", new[] { executable }.Concat(request.Args).Select(QuoteArgument)));
        if (commandLine.Length >= 32767)
            throw new DesktopException("DESKTOP_LAUNCH_BLOCKED", "Launch arguments exceed the Windows command-line limit.");
        var startup = new NativeMethods.StartupInfoEx
        {
            StartupInfo = new NativeMethods.StartupInfo { Cb = Marshal.SizeOf<NativeMethods.StartupInfo>() }
        };
        var placement = new DesktopForegroundPlacement();
        if (request.Background)
        {
            startup.StartupInfo.Flags = 1; // STARTF_USESHOWWINDOW: a hint, not a focus restriction bypass.
            startup.StartupInfo.ShowWindow = 4; // SW_SHOWNOACTIVATE.
        }
        var process = new NativeMethods.ProcessInformation();
        try
        {
            // GUI applications must never inherit this helper's gateway IPC pipes.
            // Their lifetime is independent of the launch receipt; no shell or job is involved.
            if (!NativeMethods.CreateProcessW(executable, commandLine, IntPtr.Zero, IntPtr.Zero, false,
                NativeMethods.CreateNoWindow, IntPtr.Zero, Path.GetDirectoryName(executable)!, ref startup, out process))
                throw new DesktopException("DESKTOP_LAUNCH_FAILED", new Win32Exception(Marshal.GetLastWin32Error()).Message);
            if (request.Background) placement.ObserveLaunch(checked((int)process.ProcessId), cancellation);
            // The executable may delegate to an existing app. Never attribute another process's window to this launch.
            nint foregroundAfter = DesktopNativeMethods.GetForegroundWindow();
            DesktopNativeMethods.GetWindowThreadProcessId(foregroundAfter, out uint foregroundProcess);
            return new() { ["processId"] = checked((int)process.ProcessId), ["appPath"] = executable, ["args"] = request.Args,
                ["windowDiscoveryRequired"] = true, ["backgroundRequested"] = request.Background,
                ["backgroundMode"] = request.Background ? "best-effort-no-activate" : "normal",
                ["backgroundWindowObserved"] = placement.WindowObserved,
                ["backgroundPlacementConfirmed"] = placement.WindowPlacedBehind,
                ["focusRestoreAttempted"] = placement.FocusRestoreAttempted,
                ["focusRestoreSucceeded"] = placement.FocusRestoreSucceeded,
                ["isForeground"] = foregroundProcess == process.ProcessId,
                ["foregroundPreserved"] = placement.ForegroundPreserved, ["foregroundObservedAt"] = "launch-return" };
        }
        finally
        {
            NativeMethods.Close(ref process.Thread);
            NativeMethods.Close(ref process.Process);
        }
    }

    private static string QuoteArgument(string argument)
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

    private static bool Blocked(string path)
    {
        if (!string.Equals(Path.GetExtension(path), ".exe", StringComparison.OrdinalIgnoreCase)) return true;
        string name = Path.GetFileNameWithoutExtension(path);
        if (BlockedNames.Contains(name) || name.StartsWith("python", StringComparison.OrdinalIgnoreCase)) return true;
        try
        {
            string? original = FileVersionInfo.GetVersionInfo(path).OriginalFilename;
            return original is not null && (BlockedNames.Contains(Path.GetFileNameWithoutExtension(original))
                || Path.GetFileNameWithoutExtension(original).StartsWith("python", StringComparison.OrdinalIgnoreCase));
        }
        catch (IOException) { return true; }
    }

    private static bool GuiExecutable(string path)
    {
        // PE subsystem 2 is a GUI app. Consoles and script launchers use the separately approved terminal channels.
        using FileStream stream = new(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
        using var reader = new BinaryReader(stream);
        if (stream.Length < 96 || reader.ReadUInt16() != 0x5a4d) return false;
        stream.Position = 0x3c; int offset = reader.ReadInt32();
        if (offset < 0 || offset > stream.Length - 94) return false;
        stream.Position = offset;
        if (reader.ReadUInt32() != 0x00004550) return false;
        stream.Position = offset + 24; ushort magic = reader.ReadUInt16();
        if (magic is not 0x10b and not 0x20b) return false;
        stream.Position = offset + 24 + 68;
        return reader.ReadUInt16() == 2;
    }
}
