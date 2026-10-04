using System.Text.Json;
using Microsoft.UI.Xaml.Media.Imaging;

namespace KYNXA_Desktop.Services;

/// <summary>One validated archive block; resizing reuses its bounded bytes instead of requesting the tool again.</summary>
public sealed class ArchivedScreenshot
{
    private readonly JsonElement _image;

    internal ArchivedScreenshot(JsonElement image) => _image = image.Clone();

    public Task<DecodedToolImage> DecodeAsync(int maximumPreviewDimension, CancellationToken cancellationToken) =>
        ToolResultImageDecoder.DecodeWithDimensionsAsync(_image, cancellationToken, pngOnly: _image.GetProperty("mimeType").GetString() == "image/png",
            maximumImageBytes: 4 * 1024 * 1024, maximumPixels: 16_000_000, maximumPreviewDimension: maximumPreviewDimension);
}

/// <summary>Reads one current-conversation result through the existing archive API; no model or filesystem fallback.</summary>
public static class ConversationScreenshotLoader
{
    public static async Task<BitmapImage> LoadAsync(IAgentApi api, ConversationScreenshotSource source, CancellationToken cancellationToken)
    {
        var archive = await LoadArchiveAsync(api, source, cancellationToken);
        return (await archive.DecodeAsync(1024, cancellationToken)).Bitmap;
    }

    public static async Task<ArchivedScreenshot> LoadArchiveAsync(IAgentApi api, ConversationScreenshotSource source, CancellationToken cancellationToken)
    {
        if (source.ConversationId == Guid.Empty || source.MessageId == Guid.Empty || !ConversationScreenshotSources.IsScreenshotTool(source.Tool.Name) ||
            source.Tool.Status != "completed" || !ConversationScreenshotSources.IsValidReference(source.Tool.ResultRef))
            throw new InvalidDataException("Invalid screenshot receipt.");
        var response = await api.GetToolResultAsync(source.ConversationId, source.Tool.ResultRef!, cancellationToken);
        cancellationToken.ThrowIfCancellationRequested();
        if (response.Result.ValueKind != JsonValueKind.Object || response.Result.GetRawText().Length > ConversationScreenshotSources.MaximumResultBytes ||
            !response.Result.TryGetProperty("content", out var blocks) || blocks.ValueKind != JsonValueKind.Array)
            throw new InvalidDataException("Invalid screenshot archive.");
        int examined = 0;
        foreach (var block in blocks.EnumerateArray())
        {
            if (++examined > 20) break;
            if (block.ValueKind != JsonValueKind.Object || !block.TryGetProperty("type", out var type) || type.ValueKind != JsonValueKind.String || type.GetString() != "image" ||
                !block.TryGetProperty("mimeType", out var mime) || mime.ValueKind != JsonValueKind.String || mime.GetString() is not ("image/png" or "image/jpeg")) continue;
            return new ArchivedScreenshot(block);
        }
        throw new InvalidDataException("The screenshot archive contains no typed PNG or JPEG.");
    }
}
