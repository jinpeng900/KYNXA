using System.Text;
using Windows.Storage;
using Windows.Storage.Pickers;

namespace KYNXA_Desktop.Services;

public sealed record WorkspaceExportAsset(string Filename, string MimeType, byte[] Bytes);

/// <summary>Exports explicitly requested workspace results through the native save picker.</summary>
public static class WorkspaceAssetExport
{
    public const int MaximumBytes = 64 * 1024 * 1024;
    private static int _pickerActive;
    private static readonly UTF8Encoding Utf8 = new(false, true);
    private static readonly Dictionary<string, string[]> Extensions = new(StringComparer.OrdinalIgnoreCase)
    {
        ["image/png"] = [".png"], ["image/jpeg"] = [".jpg", ".jpeg"], ["image/webp"] = [".webp"],
        ["image/svg+xml"] = [".svg"], ["text/plain"] = [".txt", ".srt", ".vtt", ".log"],
        ["text/csv"] = [".csv"], ["text/tab-separated-values"] = [".tsv"],
        ["text/markdown"] = [".md", ".markdown"], ["application/json"] = [".json"],
        ["text/html"] = [".html", ".htm"], ["text/css"] = [".css"],
        ["text/javascript"] = [".js"], ["application/javascript"] = [".js"],
        ["text/vtt"] = [".vtt"], ["application/x-subrip"] = [".srt"],
        ["model/gltf+json"] = [".gltf"], ["model/gltf-binary"] = [".glb"], ["application/zip"] = [".zip"]
    };

    public static async Task<WorkspaceExportAsset> ValidateAsync(string filename, string mimeType, string content, string encoding)
    {
        if (string.IsNullOrWhiteSpace(filename) || filename.Length > 180 || filename != Path.GetFileName(filename)
            || filename.IndexOfAny(Path.GetInvalidFileNameChars()) >= 0 || filename.EndsWith('.') || filename.EndsWith(' ')
            || !Extensions.TryGetValue(mimeType, out var accepted)
            || !accepted.Contains(Path.GetExtension(filename), StringComparer.OrdinalIgnoreCase))
            throw new InvalidDataException("导出文件名或格式无效。");
        byte[] bytes;
        if (encoding == "base64")
        {
            if (content.Length > ((MaximumBytes + 2L) / 3) * 4) throw new InvalidDataException("导出内容超过 64 MiB。");
            bytes = Convert.FromBase64String(content);
        }
        else if (encoding == "utf8")
        {
            if (content.Length > MaximumBytes || Utf8.GetByteCount(content) > MaximumBytes)
                throw new InvalidDataException("导出内容超过 64 MiB。");
            bytes = Utf8.GetBytes(content);
        }
        else throw new InvalidDataException("导出编码无效。");
        if (bytes.Length == 0 || bytes.Length > MaximumBytes) throw new InvalidDataException("导出内容为空或超过 64 MiB。");
        if (mimeType is "image/png" or "image/svg+xml")
            await DiagramExport.ValidateAsync(mimeType == "image/png" ? "png" : "svg", Convert.ToBase64String(bytes));
        else if (mimeType == "image/jpeg" && (bytes.Length < 4 || bytes[0] != 0xff || bytes[1] != 0xd8 || bytes[^2] != 0xff || bytes[^1] != 0xd9)
            || mimeType == "image/webp" && (bytes.Length < 12 || !bytes.AsSpan(0, 4).SequenceEqual("RIFF"u8) || !bytes.AsSpan(8, 4).SequenceEqual("WEBP"u8)))
            throw new InvalidDataException("图片内容与导出格式不匹配。");
        else if (mimeType == "application/zip")
        {
            if (bytes.Length < 4 || bytes[0] != 0x50 || bytes[1] != 0x4b
                || !(bytes[2] == 3 && bytes[3] == 4 || bytes[2] == 5 && bytes[3] == 6 || bytes[2] == 7 && bytes[3] == 8))
                throw new InvalidDataException("ZIP 内容与导出格式不匹配。");
        }
        else if (!mimeType.StartsWith("image/", StringComparison.Ordinal) && mimeType != "model/gltf-binary")
            _ = Utf8.GetString(bytes);
        return new(filename, mimeType, bytes);
    }

    public static async Task<DiagramExportOutcome> SaveAsync(WorkspaceExportAsset asset, nint owner)
    {
        if (owner == 0 || Interlocked.CompareExchange(ref _pickerActive, 1, 0) != 0) return DiagramExportOutcome.Error;
        try
        {
            var picker = new FileSavePicker
            {
                SuggestedStartLocation = PickerLocationId.DocumentsLibrary,
                SuggestedFileName = Path.GetFileNameWithoutExtension(asset.Filename)
            };
            string extension = Path.GetExtension(asset.Filename).ToLowerInvariant();
            picker.FileTypeChoices.Add(extension.TrimStart('.').ToUpperInvariant(), [extension]);
            WinRT.Interop.InitializeWithWindow.Initialize(picker, owner);
            var file = await picker.PickSaveFileAsync();
            if (file is null) return DiagramExportOutcome.Cancelled;
            if (!file.FileType.Equals(extension, StringComparison.OrdinalIgnoreCase)) throw new InvalidDataException("保存的文件扩展名不匹配。");
            // Commit a storage transaction, preserving the previous selected file if the write fails.
            using var transaction = await file.OpenTransactedWriteAsync();
            using (var writer = new Windows.Storage.Streams.DataWriter(transaction.Stream))
            {
                transaction.Stream.Size = 0;
                writer.WriteBytes(asset.Bytes);
                await writer.StoreAsync();
                await writer.FlushAsync();
                writer.DetachStream();
            }
            await transaction.CommitAsync();
            return DiagramExportOutcome.Success;
        }
        finally { Volatile.Write(ref _pickerActive, 0); }
    }
}
