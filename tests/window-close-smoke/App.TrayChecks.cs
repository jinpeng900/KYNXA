using System.Runtime.InteropServices;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;

namespace WindowCloseSmoke;

public partial class App
{
    private async Task CheckTrayLifecycleAsync()
    {
        var store = new FakeProjectStore();
        var shell = await OpenShellAsync(store);
        var page = shell.Page!;
        string draft = page.DraftEditor.Text;
        nint handle = WinRT.Interop.WindowNative.GetWindowHandle(shell.Window);
        using var tray = new MainWindowTrayService(shell.Window);
        int ticks = 0;
        var timer = shell.Window.DispatcherQueue.CreateTimer();
        timer.Interval = TimeSpan.FromMilliseconds(30);
        timer.Tick += (_, _) => ticks++;
        timer.Start();
        try
        {
            NativeUi.RequestClose(shell.Window);
            await WaitAsync(() => !shell.Window.AppWindow.IsVisible, "main WM_CLOSE hides the existing window in the notification area");
            int hiddenTicks = ticks;
            await WaitAsync(() => ticks > hiddenTicks + 2, "background dispatcher work continues while the main window is in the tray");
            Check(!shell.IsClosed && page.PauseCalls == 0 && store.SaveCalls == 0 && !page.IsChatClosing && page.IsEnabled,
                "tray close bypasses production reply pause, exit persistence and disposal");
            NativeUi.RequestClose(shell.Window);
            await Task.Delay(80);
            Check(page.PauseCalls == 0 && store.SaveCalls == 0 && !shell.IsClosed,
                "repeated tray closes preserve one live shell and do not replay operations");
            SendMessage(handle, 0x8000 + 73, 0, 0x0400);
            await WaitAsync(() => shell.Window.AppWindow.IsVisible, "the native tray activation callback restores the original window");
            Check(ReferenceEquals(shell.Window.Content, page) && page.DraftEditor.Text == draft &&
                WinRT.Interop.WindowNative.GetWindowHandle(shell.Window) == handle,
                "tray restoration preserves the same shell, draft and native window identity");

            var secondary = await OpenSecondaryAsync(shell);
            NativeUi.RequestClose(secondary.Window);
            await WaitAsync(() => secondary.IsClosed, "a secondary window still closes normally instead of entering the main tray");
            Check(!shell.IsClosed && shell.Window.AppWindow.IsVisible && page.PauseCalls == 0,
                "secondary close leaves the main conversation and running work untouched");

            NativeUi.RequestClose(shell.Window);
            await WaitAsync(() => !shell.Window.AppWindow.IsVisible, "the restored window can return to the notification area");
            var identifier = new TrayIconIdentifier
            {
                Size = (uint)Marshal.SizeOf<TrayIconIdentifier>(), Window = handle, Id = 1
            };
            Check(ShellNotifyIconGetRect(ref identifier, out _) == 0, "the hidden main window has a real registered notification icon");
            var icon = new TrayIconDeleteData { Size = (uint)Marshal.SizeOf<TrayIconDeleteData>(), Window = handle, Id = 1 };
            Check(ShellNotifyIcon(2, ref icon), "the fixture removes only its own icon to simulate an Explorer notification-area reset");
            SendMessage(handle, RegisterWindowMessage("TaskbarCreated"), 0, 0);
            Check(ShellNotifyIconGetRect(ref identifier, out _) == 0 && !shell.Window.AppWindow.IsVisible,
                "Explorer restart re-registers the tray icon without losing or reopening the hidden shell");

            tray.RequestExit();
            await WaitAsync(() => store.SaveCalls == 1 && page.PauseCalls == 1,
                "explicit tray Exit enters the existing production save and reply-settlement flow once");
            Check(!shell.IsClosed, "explicit exit keeps the shell alive until persistence settles");
            store.CompleteSave();
            await WaitAsync(() => shell.IsClosed, "successful explicit tray Exit closes the main window after saving");
            Check(page.WorkspaceTerminal.CloseCalls == 1 && page.DocumentPanel.CloseCalls == 1,
                "explicit exit reuses owned terminal and document cleanup");
            Check(ShellNotifyIconGetRect(ref identifier, out _) != 0, "actual main-window exit disposes its notification icon");
        }
        finally { timer.Stop(); }
    }

    private async Task CheckTrayExitRecoveryAsync()
    {
        var store = new FakeProjectStore { SaveFailure = new IOException("synthetic tray exit save failure") };
        var shell = await OpenShellAsync(store);
        var page = shell.Page!;
        using var tray = new MainWindowTrayService(shell.Window);
        StoragePaths.IsMigrating = true;
        try
        {
            tray.RequestExit();
            await Task.Delay(100);
            Check(shell.Window.AppWindow.IsVisible && store.SaveCalls == 0 && !shell.IsClosed,
                "explicit tray Exit respects the existing storage-migration veto");
            NativeUi.RequestClose(shell.Window);
            await WaitAsync(() => !shell.Window.AppWindow.IsVisible,
                "a synchronous exit veto resets tray behavior instead of making future X clicks exit");
        }
        finally { StoragePaths.IsMigrating = false; }
        tray.RequestExit();
        await WaitAsync(() => shell.Window.AppWindow.IsVisible && NativeUi.OpenDialog(page) is not null,
            "failed explicit exit restores the original shell and its save-error dialog");
        NativeUi.InvokeDialogButton(NativeUi.OpenDialog(page)!, primary: false);
        await WaitAsync(() => !page.IsClosing && page.IsEnabled, "returning from a failed explicit exit restores normal interaction");
        NativeUi.RequestClose(shell.Window);
        await WaitAsync(() => !shell.Window.AppWindow.IsVisible,
            "after an asynchronous exit failure the next X returns to the tray without retrying the failed save");
        Check(store.SaveCalls == 1 && !shell.IsClosed, "tray close does not repeat the rejected exit transaction");
        store.SaveFailure = null;
        store.CompleteSave();
        tray.RequestExit();
        await WaitAsync(() => shell.IsClosed && store.SaveCalls == 2,
            "a later explicit Exit can save and close normally after the failure is corrected");
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct TrayIconIdentifier
    {
        public uint Size;
        public nint Window;
        public uint Id;
        public Guid ItemGuid;
    }

    // NIM_DELETE reads only the identity fields; use the same current shell ABI size as the real icon structure.
    // 删除测试图标只读取身份字段；总大小仍与当前通知图标结构的原生布局一致。
    [StructLayout(LayoutKind.Sequential, Size = 976)]
    private struct TrayIconDeleteData { public uint Size; public nint Window; public uint Id; }

    [StructLayout(LayoutKind.Sequential)]
    private struct NativeRectangle { public int Left; public int Top; public int Right; public int Bottom; }

    [DllImport("user32.dll", EntryPoint = "SendMessageW")]
    private static extern nint SendMessage(nint window, uint message, nint wParam, nint lParam);
    [DllImport("user32.dll", EntryPoint = "RegisterWindowMessageW", CharSet = CharSet.Unicode)]
    private static extern uint RegisterWindowMessage(string message);
    [DllImport("shell32.dll", EntryPoint = "Shell_NotifyIconGetRect")]
    private static extern int ShellNotifyIconGetRect(ref TrayIconIdentifier identifier, out NativeRectangle rectangle);
    [DllImport("shell32.dll", EntryPoint = "Shell_NotifyIconW")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ShellNotifyIcon(uint message, ref TrayIconDeleteData data);
}
