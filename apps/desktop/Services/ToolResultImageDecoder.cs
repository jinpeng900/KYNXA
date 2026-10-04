using System.Runtime.InteropServices.WindowsRuntime;
using System.Text.Json;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Graphics.Imaging;

namespace KYNXA_Desktop.Services;

public sealed record DecodedToolImage(BitmapImage Bitmap, uint OriginalPixelWidth, uint OriginalPixelHeight);

/// <summary>Shared bounded image decoding. Thumbnails limit decoded pixels as well as the archived payload.</summary>
public static class ToolResultImageDecoder
{
    public static async Task<BitmapImage> DecodeAsync(JsonElement block, CancellationToken cancellationToken = default,
        bool pngOnly = false, int maximumImageBytes = 8 * 1024 * 1024, long maximumPixels = 16 * 1024 * 1024,
        int maximumPreviewDimension = 0) =>
        (await DecodeWithDimensionsAsync(block, cancellationToken, pngOnly, maximumImageBytes, maximumPixels, maximumPreviewDimension)).Bitmap;

    public static async Task<DecodedToolImage> DecodeWithDimensionsAsync(JsonElement block, CancellationToken cancellationToken = default,
        bool pngOnly = false, int maximumImageBytes = 8 * 1024 * 1024, long maximumPixels = 16 * 1024 * 1024,
        int maximumPreviewDimension = 0)
    {
        string mime = StringField(block, "mimeType"), encoded = StringField(block, "data");
        if (pngOnly ? mime != "image/png" : mime is not ("image/png" or "image/jpeg" or "image/gif" or "image/webp"))
            throw new InvalidDataException("Unsupported tool result image type.");
        if (encoded.Length == 0 || encoded.Length > ((maximumImageBytes + 2L) / 3) * 4)
            throw new InvalidDataException("Tool result image is too large.");
        cancellationToken.ThrowIfCancellationRequested();
        byte[] bytes = Convert.FromBase64String(encoded);
        if (bytes.Length > maximumImageBytes || (pngOnly &&
            (!bytes.AsSpan().StartsWith(new byte[] { 137, 80, 78, 71, 13, 10, 26, 10 }) || Convert.ToBase64String(bytes) != encoded)))
            throw new InvalidDataException("Invalid tool result PNG.");
        using var stream = new MemoryStream(bytes).AsRandomAccessStream();
        var decoder = await BitmapDecoder.CreateAsync(stream).AsTask(cancellationToken);
        if (decoder.PixelWidth == 0 || decoder.PixelHeight == 0 || (long)decoder.PixelWidth * decoder.PixelHeight > maximumPixels)
            throw new InvalidDataException("Tool result image dimensions are too large.");
        stream.Seek(0);
        var bitmap = new BitmapImage();
        if (maximumPreviewDimension > 0)
        {
            double scale = Math.Min(1, maximumPreviewDimension / (double)Math.Max(decoder.PixelWidth, decoder.PixelHeight));
            bitmap.DecodePixelType = DecodePixelType.Physical;
            bitmap.DecodePixelWidth = Math.Max(1, (int)Math.Floor(decoder.PixelWidth * scale));
            bitmap.DecodePixelHeight = Math.Max(1, (int)Math.Floor(decoder.PixelHeight * scale));
        }
        await bitmap.SetSourceAsync(stream).AsTask(cancellationToken);
        cancellationToken.ThrowIfCancellationRequested();
        return new(bitmap, decoder.PixelWidth, decoder.PixelHeight);
    }

    private static string StringField(JsonElement value, string name) => value.ValueKind == JsonValueKind.Object &&
        value.TryGetProperty(name, out var field) && field.ValueKind == JsonValueKind.String ? field.GetString() ?? "" : "";
}
