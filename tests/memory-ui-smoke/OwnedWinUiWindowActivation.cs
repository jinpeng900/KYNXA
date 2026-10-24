using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;

namespace MemoryUiSmoke;

internal static class OwnedWinUiWindowActivation
{
    public static async Task EnsureAsync(Window window, string? diagnosticPath = null)
    {
        var work = DisplayArea.GetFromWindowId(window.AppWindow.Id, DisplayAreaFallback.Primary).WorkArea;
        var size = window.AppWindow.Size;
        var position = window.AppWindow.Position;
        int width = Math.Min(size.Width, work.Width - 32), height = Math.Min(size.Height, work.Height - 32);
        if (position.X < work.X || position.Y < work.Y || position.X + size.Width > work.X + work.Width || position.Y + size.Height > work.Y + work.Height)
            window.AppWindow.MoveAndResize(new Windows.Graphics.RectInt32(work.X + 16, work.Y + 16, width, height));
        var presenter = window.AppWindow.Presenter as OverlappedPresenter
            ?? throw new InvalidOperationException("The owned fixture has no overlapped presenter.");
        if (presenter.State == OverlappedPresenterState.Minimized) presenter.Restore();
        window.AppWindow.Show(activateWindow: true);
        window.Activate();
        bool originalTopmost = presenter.IsAlwaysOnTop;
        await OwnedWindowActivation.EnsureAsync(WinRT.Interop.WindowNative.GetWindowHandle(window),
            () => { presenter.IsAlwaysOnTop = true; window.AppWindow.MoveInZOrderAtTop(); },
            () => presenter.IsAlwaysOnTop = originalTopmost, diagnosticPath);
    }
}
