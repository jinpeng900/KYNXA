using System.IO;
using System.Security.Cryptography;
using System.Text;
using System.Windows;
using Kynxa.UsageMonitor.Core;

namespace Kynxa.UsageMonitor;

public partial class App : Application
{
    private Mutex? _instanceMutex;
    private bool _ownsMutex;

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        try
        {
            MonitorOptions options = MonitorOptions.Parse(e.Args);
            string identity = Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(options.DataDirectory.ToUpperInvariant())))[..24];
            _instanceMutex = new Mutex(true, @"Local\KYNXA.UsageMonitor." + identity, out _ownsMutex);
            if (!_ownsMutex) throw new InvalidOperationException("already_running");
            Directory.CreateDirectory(options.DataDirectory);
            MainWindow window = new(options);
            MainWindow = window;
            window.Show();
        }
        catch (Exception error) when (error is ArgumentException or IOException or UnauthorizedAccessException or InvalidOperationException or System.ComponentModel.Win32Exception)
        {
            WriteControlError();
            Shutdown(1);
        }
    }

    private static void WriteControlError()
    {
        try
        {
            Console.Out.WriteLine(UsageControlReader.ErrorCommand);
            Console.Out.Flush();
        }
        catch (IOException) { }
    }

    protected override void OnExit(ExitEventArgs e)
    {
        if (_ownsMutex) _instanceMutex?.ReleaseMutex();
        _instanceMutex?.Dispose();
        base.OnExit(e);
    }
}

public sealed record MonitorOptions(string? UsageLogPath, string DataDirectory, int? OwnerProcessId, bool Replay, bool ControlStdin, bool PreviewTaskbar)
{
    public static MonitorOptions Parse(string[] arguments)
    {
        string? usageLog = null;
        string? dataDirectory = null;
        int? ownerProcessId = null;
        bool replay = false;
        bool controlStdin = false;
        bool previewTaskbar = false;
        for (int index = 0; index < arguments.Length; index++)
        {
            string argument = arguments[index];
            if (argument == "--replay") { replay = true; continue; }
            if (argument == "--control-stdin") { controlStdin = true; continue; }
            if (argument == "--preview-taskbar") { previewTaskbar = true; continue; }
            if (index + 1 >= arguments.Length) throw new ArgumentException("missing_value");
            string value = arguments[++index];
            switch (argument)
            {
                case "--usage-log": usageLog = value; break;
                case "--data-dir": dataDirectory = value; break;
                case "--owner-pid":
                    if (!int.TryParse(value, out int processId) || processId <= 0) throw new ArgumentException("invalid_owner");
                    ownerProcessId = processId;
                    break;
                default: throw new ArgumentException("unknown_argument");
            }
        }
        if (string.IsNullOrWhiteSpace(dataDirectory) || replay == controlStdin ||
            (replay && string.IsNullOrWhiteSpace(usageLog)) ||
            (controlStdin && (!ownerProcessId.HasValue || usageLog is not null))) throw new ArgumentException("invalid_launch_mode");
        return new MonitorOptions(usageLog is null ? null : Path.GetFullPath(usageLog),
            Path.TrimEndingDirectorySeparator(Path.GetFullPath(dataDirectory)), ownerProcessId, replay, controlStdin, previewTaskbar);
    }
}
