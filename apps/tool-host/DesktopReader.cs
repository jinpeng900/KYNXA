using System.Windows;
using System.Windows.Automation;
using System.Windows.Automation.Text;
using System.Windows.Interop;
using System.Windows.Media.Imaging;
using static KYNXA.ToolHost.DesktopNativeMethods;

namespace KYNXA.ToolHost;

internal static class DesktopReader
{
    internal static Dictionary<string, object?> Screenshot(DesktopTarget target, DesktopRegion? crop)
    {
        target.Check(); target.EnsureIntegrity();
        DesktopNativeMethods.Rect rect = target.ClientRect();
        int width = rect.Right, height = rect.Bottom;
        if (crop is not null) ValidateRegion(crop, width, height);
        if (width > 8192 || height > 8192 || (long)width * height > 16000000)
            throw new DesktopException("DESKTOP_IMAGE_TOO_LARGE", "The selected client area is too large to capture.");
        nint windowDeviceContext = GetDC(target.Window), memoryDeviceContext = 0, bitmapHandle = 0, previousBitmap = 0;
        try
        {
            if (windowDeviceContext == 0 || (memoryDeviceContext = CreateCompatibleDC(windowDeviceContext)) == 0 || (bitmapHandle = CreateCompatibleBitmap(windowDeviceContext, width, height)) == 0)
                throw new DesktopException("DESKTOP_CAPTURE_FAILED", "The selected window capture surface is unavailable.");
            previousBitmap = SelectObject(memoryDeviceContext, bitmapHandle);
            if (!PrintWindow(target.Window, memoryDeviceContext, 3))
                throw new DesktopException("DESKTOP_CAPTURE_FAILED", "The selected window did not provide a client image; no full-desktop fallback was used.");
            target.Check();
            BitmapSource image = Imaging.CreateBitmapSourceFromHBitmap(bitmapHandle, 0, Int32Rect.Empty, BitmapSizeOptions.FromEmptyOptions());
            if (crop is not null)
            {
                image = new CroppedBitmap(image, new Int32Rect(crop.X, crop.Y, crop.Width, crop.Height));
            }
            var encoder = new PngBitmapEncoder(); encoder.Frames.Add(BitmapFrame.Create(image));
            using var output = new MemoryStream(); encoder.Save(output);
            if (output.Length > 4 * 1024 * 1024)
                throw new DesktopException("DESKTOP_IMAGE_TOO_LARGE", "The PNG exceeds the 4 MiB capture limit.");
            return new() { ["windowId"] = target.Window.ToInt64().ToString(), ["processId"] = target.ProcessId,
                ["mimeType"] = "image/png", ["data"] = Convert.ToBase64String(output.ToArray()), ["width"] = image.PixelWidth, ["height"] = image.PixelHeight,
                ["clientWidth"] = width, ["clientHeight"] = height, ["crop"] = crop,
                ["captureMethod"] = "print-window-client", ["coordinates"] = "client-physical-pixels" };
        }
        finally
        {
            if (previousBitmap != 0 && memoryDeviceContext != 0) SelectObject(memoryDeviceContext, previousBitmap);
            if (bitmapHandle != 0) DeleteObject(bitmapHandle);
            if (memoryDeviceContext != 0) DeleteDC(memoryDeviceContext);
            if (windowDeviceContext != 0) ReleaseDC(target.Window, windowDeviceContext);
        }
    }

    internal static Dictionary<string, object?> Read(DesktopTarget target, DesktopRequest request, CancellationToken cancellation)
    {
        if (request.MaxCharacters is < 1 or > 64000 || request.MaxElements is < 1 or > 1000)
            throw new DesktopException("DESKTOP_INVALID_REQUEST", "Read limits are outside the supported bounds.");
        if (request.ElementId is { } requestedId && (requestedId.Length is 0 or > 256 ||
            requestedId.Any(character => !char.IsAsciiDigit(character) && character is not ',' and not '-')))
            throw new DesktopException("DESKTOP_INVALID_REQUEST", "The element identifier is invalid.");
        target.Check(); target.EnsureIntegrity(); target.EnsureResponding();
        var origin = new DesktopNativeMethods.Point(); ClientToScreen(target.Window, ref origin);
        DesktopNativeMethods.Rect client = target.ClientRect();
        var clientBounds = new System.Windows.Rect(origin.X, origin.Y, client.Right, client.Bottom);
        if (request.Region is { } region)
        {
            ValidateRegion(region, client.Right, client.Bottom);
            clientBounds = new System.Windows.Rect(origin.X + region.X, origin.Y + region.Y, region.Width, region.Height);
        }
        var text = new System.Text.StringBuilder(); var elements = new List<object>(); var urls = new HashSet<string>();
        var seenText = new HashSet<string>(StringComparer.Ordinal); bool truncated = false, foundElement = request.ElementId is null; int visited = 0;
        void Append(string? value)
        {
            if (string.IsNullOrWhiteSpace(value)) return;
            string normalized = value.Trim();
            if (!seenText.Add(normalized)) return;
            int remaining = request.MaxCharacters - text.Length;
            if (remaining <= 0) { truncated = true; return; }
            string visible = normalized[..Math.Min(remaining, normalized.Length)];
            text.Append(visible); if (text.Length < request.MaxCharacters) text.Append('\n');
            if (visible.Length < normalized.Length) truncated = true;
        }
        void Address(string? value)
        {
            if (value is null || value.Length > 8192 || !Uri.TryCreate(value.Trim(), UriKind.Absolute, out Uri? uri)
                || uri.Scheme is not "http" and not "https" || !string.IsNullOrEmpty(uri.UserInfo)) return;
            urls.Add(uri.AbsoluteUri);
        }
        void Visit(AutomationElement element, int depth, bool selected)
        {
            cancellation.ThrowIfCancellationRequested();
            if (++visited > 4096 || depth > 24 || elements.Count >= request.MaxElements) { truncated = true; return; }
            try
            {
                var current = element.Current;
                if (current.IsOffscreen) return;
                System.Windows.Rect bounds = current.BoundingRectangle;
                if (bounds.IsEmpty || !bounds.IntersectsWith(clientBounds)) return;
                string elementId = string.Join(',', element.GetRuntimeId());
                selected |= elementId == request.ElementId;
                if (selected) foundElement = true;
                bool password = current.IsPassword;
                // IsPassword is checked before Name, Value or TextPattern: even an unsafe
                // provider's password Name must never enter the returned metadata.
                // 在读取 Name、Value 或 TextPattern 前先检查 IsPassword，避免不安全提供方将密码控件名称带入返回元数据。
                string name = selected && !password ? current.Name ?? "" : "", value = "";
                if (selected && !password && element.TryGetCurrentPattern(TextPattern.Pattern, out object rawText))
                    foreach (TextPatternRange range in ((TextPattern)rawText).GetVisibleRanges().Take(64))
                    {
                        if (!range.GetBoundingRectangles().Any(box => !box.IsEmpty && box.IntersectsWith(clientBounds))) continue;
                        string content = range.GetText(Math.Max(1, request.MaxCharacters - text.Length));
                        Append(content);
                    }
                if (selected && !password && element.TryGetCurrentPattern(ValuePattern.Pattern, out object rawValue))
                    value = ((ValuePattern)rawValue).Current.Value ?? "";
                Append(name); Append(value); Address(value);
                if (selected) elements.Add(new { name = name[..Math.Min(256, name.Length)], value = value[..Math.Min(1024, value.Length)],
                    controlType = current.ControlType.ProgrammaticName, automationId = Limit(current.AutomationId, 128), elementId,
                    x = Math.Max(0, bounds.X - origin.X), y = Math.Max(0, bounds.Y - origin.Y), width = bounds.Width, height = bounds.Height,
                    isEnabled = current.IsEnabled, isPassword = password });
                if (password) return;
                if (text.Length >= request.MaxCharacters) { truncated = true; return; }
                var walker = TreeWalker.ControlViewWalker;
                for (AutomationElement? child = walker.GetFirstChild(element); child is not null; child = walker.GetNextSibling(child))
                {
                    Visit(child, depth + 1, selected);
                    if (elements.Count >= request.MaxElements || visited > 4096 || text.Length >= request.MaxCharacters) { truncated = true; break; }
                }
            }
            catch (ElementNotAvailableException) { truncated = true; }
        }
        Visit(AutomationElement.FromHandle(target.Window), 0, request.ElementId is null);
        if (!foundElement) throw new DesktopException("DESKTOP_ELEMENT_NOT_FOUND", "The accessible element no longer exists in the selected window or region.");
        target.Check();
        return new() { ["windowId"] = target.Window.ToInt64().ToString(), ["processId"] = target.ProcessId,
            ["source"] = "uia-visible", ["text"] = text.ToString().TrimEnd(), ["elements"] = elements, ["accessibleUrls"] = urls.ToArray(),
            ["truncated"] = truncated, ["clientWidth"] = client.Right, ["clientHeight"] = client.Bottom,
            ["region"] = request.Region, ["elementId"] = request.ElementId, ["timeoutMs"] = request.TimeoutMs,
            ["limitations"] = new[] { "Accessible visible content only; not a browser DOM or full document. Regions select intersecting accessible elements/ranges.", "Password controls expose location metadata only, without Name, Value or text; providers may omit content." } };
    }

    private static string Limit(string? value, int maximum) => value is null ? "" : value[..Math.Min(maximum, value.Length)];

    private static void ValidateRegion(DesktopRegion region, int width, int height)
    {
        if (region.X < 0 || region.Y < 0 || region.Width <= 0 || region.Height <= 0 ||
            (long)region.X + region.Width > width || (long)region.Y + region.Height > height)
            throw new DesktopException("DESKTOP_INVALID_COORDINATES", "The region must fit inside the selected physical client area.");
    }
}
