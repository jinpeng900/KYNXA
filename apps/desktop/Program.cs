#if DISABLE_XAML_GENERATED_MAIN
using System.Diagnostics;
using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;
using KYNXA_Desktop.Services;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.Windows.AppLifecycle;

namespace KYNXA_Desktop;

/// <summary>
/// 在构造 XAML 或启动服务前，将重复启动交回已有桌面实例。
/// </summary>
internal static class Program
{
    private const uint RedirectTimeoutMs = 10_000;
    private static DispatcherQueue? _dispatcher;

    [STAThread]
    private static int Main(string[] args)
    {
        WinRT.ComWrappersSupport.InitializeComWrappers();
        var current = AppInstance.GetCurrent();
        current.Activated += InstanceActivated;
        try
        {
            // AppInstance 按应用身份隔离此键，独立打包的测试版本不会占用正式应用实例。
            var primary = AppInstance.FindOrRegisterForKey(AppIdentity.AppUserModelId + ".MainWindow");
            if (!primary.IsCurrent)
            {
                try
                {
                    AllowSetForegroundWindow(primary.ProcessId);
                    RedirectActivation(primary, current.GetActivatedEventArgs());
                    return 0;
                }
                catch (Exception error)
                {
                    // 主实例在转交期间退出时，副实例安静结束，不另开界面或触发崩溃调试器。
                    Debug.WriteLine($"Desktop activation redirection failed: {error.GetType().Name}");
                    return 1;
                }
            }
            Application.Start(initialization =>
            {
                _dispatcher = DispatcherQueue.GetForCurrentThread();
                SynchronizationContext.SetSynchronizationContext(new DispatcherQueueSynchronizationContext(_dispatcher));
                _ = new App();
            });
            return 0;
        }
        finally
        {
            current.Activated -= InstanceActivated;
            _dispatcher = null;
        }
    }

    private static void InstanceActivated(object? sender, AppActivationArguments args)
    {
        // XAML 启动前收到激活时，首次启动本身就会显示窗口，无需额外建窗。
        _dispatcher?.TryEnqueue(() =>
        {
            if (App.Window is MainWindow existingWindow) existingWindow.RestoreFromActivation();
        });
    }

    private static void RedirectActivation(AppInstance primary, AppActivationArguments args)
    {
        var completed = new ManualResetEvent(false);
        ExceptionDispatchInfo? failure = null;
        var redirect = Task.Run(async () =>
        {
            try { await primary.RedirectActivationToAsync(args); }
            catch (Exception error) { failure = ExceptionDispatchInfo.Capture(error); }
            finally { completed.Set(); }
        });
        // MTA 转交激活时，启动 STA 继续调度 COM 调用；直接等待 Task 可能使激活死锁。
        try
        {
            int result = CoWaitForMultipleObjects(0, RedirectTimeoutMs, 1, [completed.SafeWaitHandle.DangerousGetHandle()], out _);
            Marshal.ThrowExceptionForHR(result);
            failure?.Throw();
        }
        finally
        {
            // 即使原生等待失败，也保留事件至工作线程完成通知后再释放。
            if (redirect.IsCompleted) completed.Dispose();
            else _ = redirect.ContinueWith(task => completed.Dispose(), TaskScheduler.Default);
        }
    }

    [DllImport("ole32.dll", ExactSpelling = true)]
    private static extern int CoWaitForMultipleObjects(uint flags, uint timeoutMs, uint count, nint[] handles, out uint index);

    [DllImport("user32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool AllowSetForegroundWindow(uint processId);
}
#endif
