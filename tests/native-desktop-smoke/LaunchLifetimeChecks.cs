using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text.Json;

internal static partial class Program
{
    private static async Task CheckLaunchLifetimeAsync()
    {
        string directory = Path.Combine(ArtifactFolder, "independent-launch");
        using var helper = new Process
        {
            StartInfo = new ProcessStartInfo(Host)
            {
                UseShellExecute = false, CreateNoWindow = true,
                RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true
            }
        };
        nint job = CreateJobObjectW(0, null);
        if (job == 0) throw new Win32Exception(Marshal.GetLastWin32Error());
        int childId = 0;
        bool helperStarted = false;
        try
        {
            // Only this launch helper permits explicit breakaway. Ordinary child creation remains owned by the job.
            // 仅本启动助手允许显式脱离作业；普通子进程创建仍受自有作业管理。
            var limits = new LaunchJobLimits { Basic = new LaunchJobBasicLimits { Flags = 0x2000 | 0x0800 } };
            Check(SetInformationJobObject(job, 9, ref limits, (uint)Marshal.SizeOf<LaunchJobLimits>()),
                "Launch helper receives a bounded kill-on-close job with explicit breakaway permission");
            helperStarted = helper.Start();
            Check(helperStarted, "Only the owned launch helper is started");
            Check(AssignProcessToJobObject(job, helper.Handle), "Helper belongs to this fixture's lifecycle job before dispatch");
            Check(IsProcessInJob(helper.Handle, job, out bool helperInJob) && helperInJob,
                "Actual helper job membership is confirmed");
            await helper.StandardInput.WriteLineAsync(JsonSerializer.Serialize(new
            {
                operation = "desktop", action = "launch", appPath = Environment.ProcessPath,
                args = new[] { "--background-fixture", directory }, background = true
            }, JsonOptions));
            await helper.StandardInput.FlushAsync();
            string response = await helper.StandardOutput.ReadLineAsync().WaitAsync(TimeSpan.FromSeconds(20))
                ?? throw new Exception("Launch helper returned no receipt.");
            JsonElement receipt = JsonDocument.Parse(response).RootElement.Clone();
            Check(receipt.TryGetProperty("completed", out JsonElement completed) && completed.GetBoolean(),
                "Independent application launch returns an actual completion receipt: " + response);
            childId = receipt.GetProperty("processId").GetInt32();
            await helper.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5));
            Check(helper.HasExited, "Launch helper exits before application liveness is checked");
            Check(CloseHandle(job), "Closing the fixture's job confirms the real kill-on-close boundary");
            job = 0;
            await Task.Delay(350);
            using var child = Process.GetProcessById(childId);
            Check(!child.HasExited, "Launched GUI survives helper exit and lifecycle job disposal");
            string readyFile = Path.Combine(directory, "ready.json");
            for (int index = 0; index < 50 && !File.Exists(readyFile) && !child.HasExited; index++) await Task.Delay(100);
            Check(File.Exists(readyFile), "The independently running owned GUI reaches ready state");
            JsonElement ready = JsonDocument.Parse(await File.ReadAllTextAsync(readyFile)).RootElement.Clone();
            Check(ready.GetProperty("processId").GetInt32() == childId,
                "Independent GUI identity matches the launch receipt without browser handoff");
            JsonElement windows = await RequestAsync(new { operation = "desktop", action = "windows", processId = childId });
            Check(windows.GetProperty("windows").EnumerateArray().Any(window => window.GetProperty("processId").GetInt32() == childId),
                "Launched GUI remains discoverable after helper job cleanup");
        }
        finally
        {
            if (job != 0) CloseHandle(job);
            if (helperStarted && !helper.HasExited) { helper.Kill(entireProcessTree: true); await helper.WaitForExitAsync(); }
            if (childId != 0)
            {
                try
                {
                    using var child = Process.GetProcessById(childId);
                    if (!child.HasExited) child.Kill(entireProcessTree: true);
                    await child.WaitForExitAsync();
                }
                catch (ArgumentException) { }
            }
        }
        await CheckLaunchBreakawayDenialAsync();
    }

    private static async Task CheckLaunchBreakawayDenialAsync()
    {
        using var helper = new Process
        {
            StartInfo = new ProcessStartInfo(Host)
            {
                UseShellExecute = false, CreateNoWindow = true,
                RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true
            }
        };
        nint job = CreateJobObjectW(0, null);
        if (job == 0) throw new Win32Exception(Marshal.GetLastWin32Error());
        bool helperStarted = false;
        try
        {
            // Ordinary jobs keep ownership; denied breakaway cannot fall back to a detached host process.
            // 普通作业继续保留进程所有权；脱离被拒绝时不得回退为脱离管理的宿主进程。
            var limits = new LaunchJobLimits { Basic = new LaunchJobBasicLimits { Flags = 0x2000 } };
            Check(SetInformationJobObject(job, 9, ref limits, (uint)Marshal.SizeOf<LaunchJobLimits>()),
                "Ordinary helper jobs retain kill-on-close without breakaway permission");
            helperStarted = helper.Start();
            Check(helperStarted && AssignProcessToJobObject(job, helper.Handle),
                "Owned refusal fixture enters the ordinary job before dispatch");
            await helper.StandardInput.WriteLineAsync(JsonSerializer.Serialize(new
            {
                operation = "desktop", action = "launch", appPath = Environment.ProcessPath,
                args = new[] { "--background-fixture", Path.Combine(ArtifactFolder, "denied-launch") }, background = true
            }, JsonOptions));
            await helper.StandardInput.FlushAsync();
            string response = await helper.StandardOutput.ReadLineAsync().WaitAsync(TimeSpan.FromSeconds(20))
                ?? throw new Exception("Refusal helper returned no receipt.");
            JsonElement receipt = JsonDocument.Parse(response).RootElement.Clone();
            CheckError(receipt, "DESKTOP_LAUNCH_FAILED");
            Check(!Directory.Exists(Path.Combine(ArtifactFolder, "denied-launch")),
                "Denied breakaway creates no GUI and has no unowned fallback");
            await helper.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5));
        }
        finally
        {
            CloseHandle(job);
            if (helperStarted && !helper.HasExited) { helper.Kill(entireProcessTree: true); await helper.WaitForExitAsync(); }
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct LaunchJobBasicLimits
    {
        public long ProcessTime, JobTime;
        public uint Flags;
        public nuint MinimumWorkingSet, MaximumWorkingSet;
        public uint ActiveProcesses;
        public nuint Affinity;
        public uint Priority, SchedulingClass;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct LaunchJobLimits
    {
        public LaunchJobBasicLimits Basic;
        public ulong ReadOperations, WriteOperations, OtherOperations, ReadBytes, WriteBytes, OtherBytes;
        public nuint ProcessMemory, JobMemory, PeakProcessMemory, PeakJobMemory;
    }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern nint CreateJobObjectW(nint attributes, string? name);
    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetInformationJobObject(nint job, int informationClass, ref LaunchJobLimits limits, uint bytes);
    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool AssignProcessToJobObject(nint job, nint process);
    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsProcessInJob(nint process, nint job, [MarshalAs(UnmanagedType.Bool)] out bool belongs);
    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CloseHandle(nint handle);
}
