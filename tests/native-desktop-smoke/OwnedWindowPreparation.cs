using System.IO;
using System.Windows;
using System.Windows.Interop;
using MemoryUiSmoke;

internal static partial class Program
{
    private static async Task PrepareOwnedInputWindowAsync(Window window)
    {
        var work = SystemParameters.WorkArea;
        window.Width = Math.Min(window.Width, work.Width - 32);
        window.Height = Math.Min(window.Height, work.Height - 32);
        window.Left = Math.Clamp(window.Left, work.Left + 16, work.Right - window.Width - 16);
        window.Top = Math.Clamp(window.Top, work.Top + 16, work.Bottom - window.Height - 16);
        window.Show();
        window.Activate();
        bool originalTopmost = window.Topmost;
        await OwnedWindowActivation.EnsureAsync(new WindowInteropHelper(window).Handle,
            () => window.Topmost = true, () => window.Topmost = originalTopmost,
            Path.Combine(ArtifactFolder, "native-activation.jsonl"));
    }
}
