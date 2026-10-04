using System.Runtime.InteropServices;
using Microsoft.UI.Xaml;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;

namespace MemoryUiSmoke;

/// <summary>Captures the native window with PrintWindow and encodes the captured pixels unchanged.
/// 使用 PrintWindow 捕获原生窗口，并保持捕获像素原样编码。
/// </summary>
internal static class NativeWindowCapture
{
    public static async Task CaptureAsync(Window window, string path)
    {
        nint hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);
        if (!GetWindowRect(hwnd, out var rect)) throw new InvalidOperationException("GetWindowRect failed.");
        int width = rect.Right - rect.Left, height = rect.Bottom - rect.Top;
        nint source = GetDC(hwnd);
        nint destination = CreateCompatibleDC(source);
        nint bitmap = 0, previous = 0;
        try
        {
            var info = new BitmapInfo
            {
                Header = new BitmapInfoHeader
                {
                    Size = (uint)Marshal.SizeOf<BitmapInfoHeader>(),
                    Width = width,
                    Height = -height,
                    Planes = 1,
                    BitCount = 32,
                    Compression = 0,
                    SizeImage = checked((uint)(width * height * 4))
                }
            };
            bitmap = CreateDIBSection(destination, ref info, 0, out nint pixels, 0, 0);
            if (bitmap == 0 || pixels == 0) throw new InvalidOperationException("CreateDIBSection failed.");
            previous = SelectObject(destination, bitmap);
            if (!PrintWindow(hwnd, destination, 2)) throw new InvalidOperationException("PrintWindow failed.");
            byte[] bytes = new byte[checked(width * height * 4)];
            Marshal.Copy(pixels, bytes, 0, bytes.Length);
            using var stream = new InMemoryRandomAccessStream();
            var encoder = await BitmapEncoder.CreateAsync(BitmapEncoder.PngEncoderId, stream);
            encoder.SetPixelData(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Ignore,
                (uint)width, (uint)height, 96, 96, bytes);
            await encoder.FlushAsync();
            stream.Seek(0);
            await using var output = File.Create(path);
            await stream.AsStreamForRead().CopyToAsync(output);
        }
        finally
        {
            if (previous != 0) SelectObject(destination, previous);
            if (bitmap != 0) DeleteObject(bitmap);
            if (destination != 0) DeleteDC(destination);
            if (source != 0) ReleaseDC(hwnd, source);
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Rect { public int Left, Top, Right, Bottom; }

    [StructLayout(LayoutKind.Sequential)]
    private struct BitmapInfoHeader
    {
        public uint Size;
        public int Width, Height;
        public ushort Planes, BitCount;
        public uint Compression, SizeImage;
        public int XPelsPerMeter, YPelsPerMeter;
        public uint ColorsUsed, ColorsImportant;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct BitmapInfo { public BitmapInfoHeader Header; public uint Colors; }

    [DllImport("user32.dll")] private static extern bool GetWindowRect(nint hwnd, out Rect rect);
    [DllImport("user32.dll")] private static extern nint GetDC(nint hwnd);
    [DllImport("user32.dll")] private static extern int ReleaseDC(nint hwnd, nint dc);
    [DllImport("user32.dll")] private static extern bool PrintWindow(nint hwnd, nint dc, uint flags);
    [DllImport("gdi32.dll")] private static extern nint CreateCompatibleDC(nint dc);
    [DllImport("gdi32.dll")] private static extern bool DeleteDC(nint dc);
    [DllImport("gdi32.dll")] private static extern nint SelectObject(nint dc, nint obj);
    [DllImport("gdi32.dll")] private static extern bool DeleteObject(nint obj);
    [DllImport("gdi32.dll")] private static extern nint CreateDIBSection(nint dc, ref BitmapInfo info, uint usage,
        out nint bits, nint section, uint offset);
}
