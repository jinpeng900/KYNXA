using System.Text;
using Windows.Data.Pdf;
using Windows.Globalization;
using Windows.Graphics.Imaging;
using Windows.Media.Ocr;
using Windows.Storage.Streams;

namespace KYNXA.ToolHost;

internal sealed record DocumentOcrRequest
{
    public string Operation { get; init; } = "document_ocr";
    public string BytesBase64 { get; init; } = "";
    public int[] Pages { get; init; } = [];
    public string? Language { get; init; }
    public int MaximumOutputBytes { get; init; } = 2 * 1024 * 1024;
    public int RenderWidth { get; init; } = 1800;
}

internal sealed class DocumentOcrException(string code, string message) : Exception(message)
{
    public string Code { get; } = code;
}

/// <summary>
/// Recognize only caller-authorized PDF bytes; this offline service never reads a path or controls desktop windows.
/// 仅识别调用方已授权的 PDF 字节；离线服务不接文件路径，也不操作桌面窗口。
/// </summary>
internal static class DocumentOcr
{
    internal const int MaximumInputBytes = 32 * 1024 * 1024;
    internal const int MaximumFrameCharacters = 46 * 1024 * 1024;

    private static string BackendVersion => $"windows-media-ocr-v1|os:{Environment.OSVersion.Version}";

    internal static object Capabilities()
    {
        var languages = OcrEngine.AvailableRecognizerLanguages.Select(language => language.LanguageTag).ToArray();
        return new { protocolVersion = 1, boundary = "document-ocr", available = languages.Length > 0,
            version = BackendVersion, languages, offline = true, maximumInputBytes = MaximumInputBytes,
            maximumPages = 100, maximumImageDimension = OcrEngine.MaxImageDimension };
    }

    internal static async Task<object> RunAsync(DocumentOcrRequest request, CancellationToken cancellation)
    {
        if (request.Operation != "document_ocr" || request.BytesBase64.Length == 0 ||
            request.BytesBase64.Length > (MaximumInputBytes + 2L) / 3 * 4 || request.Pages.Length is < 1 or > 100 ||
            request.Pages.Distinct().Count() != request.Pages.Length || request.Pages.Any(page => page < 1) ||
            request.MaximumOutputBytes is < 1 or > 2 * 1024 * 1024 || request.RenderWidth is < 256 or > 4000)
            throw new DocumentOcrException("OCR_INVALID_REQUEST", "Invalid bounded PDF OCR request. / PDF OCR 请求不符合额度。");
        byte[] bytes;
        try { bytes = Convert.FromBase64String(request.BytesBase64); }
        catch (FormatException) { throw new DocumentOcrException("OCR_INVALID_REQUEST", "Invalid document bytes. / 文档字节无效。"); }
        if (bytes.Length > MaximumInputBytes || bytes.Length < 5 || Encoding.ASCII.GetString(bytes, 0, 5) != "%PDF-")
            throw new DocumentOcrException("OCR_INVALID_REQUEST", "OCR requires authorized PDF bytes. / OCR 需要已授权的 PDF 字节。");

        OcrEngine? engine;
        try
        {
            engine = request.Language is null ? OcrEngine.TryCreateFromUserProfileLanguages()
                : OcrEngine.TryCreateFromLanguage(new Language(request.Language));
        }
        catch (ArgumentException) { throw new DocumentOcrException("OCR_LANGUAGE_UNAVAILABLE", "OCR language is unavailable. / OCR 语言不可用。"); }
        if (engine is null)
            throw new DocumentOcrException("OCR_LANGUAGE_UNAVAILABLE", "No installed Windows OCR language is available. / 当前没有可用的 Windows OCR 语言包。");

        using var input = new InMemoryRandomAccessStream();
        using (var writer = new DataWriter(input))
        {
            writer.WriteBytes(bytes);
            await writer.StoreAsync().AsTask(cancellation);
            writer.DetachStream();
        }
        input.Seek(0);
        var document = await PdfDocument.LoadFromStreamAsync(input).AsTask(cancellation);
        if (request.Pages.Any(page => page > document.PageCount))
            throw new DocumentOcrException("OCR_INVALID_PAGE", "OCR page is outside the document. / OCR 页号超出文档范围。");
        var recognized = new List<object>();
        int outputBytes = 0;
        foreach (int pageNumber in request.Pages)
        {
            cancellation.ThrowIfCancellationRequested();
            using var page = document.GetPage((uint)pageNumber - 1);
            var size = page.Size;
            if (!double.IsFinite(size.Width) || !double.IsFinite(size.Height) || size.Width <= 0 || size.Height <= 0)
                throw new DocumentOcrException("OCR_INVALID_PAGE", "Invalid PDF page dimensions. / PDF 页面尺寸无效。");
            // Bound both dimensions before allocation; very long pages cannot force an unbounded bitmap.
            // 分配前限制两个方向的尺寸，极长页面不能强制创建无界位图。
            double scale = Math.Min(request.RenderWidth / size.Width,
                OcrEngine.MaxImageDimension / Math.Max(size.Width, size.Height));
            uint width = (uint)Math.Max(1, Math.Floor(size.Width * scale));
            uint height = (uint)Math.Max(1, Math.Floor(size.Height * scale));
            using var rendered = new InMemoryRandomAccessStream();
            await page.RenderToStreamAsync(rendered, new PdfPageRenderOptions
                { DestinationWidth = width, DestinationHeight = height }).AsTask(cancellation);
            rendered.Seek(0);
            var decoder = await BitmapDecoder.CreateAsync(rendered).AsTask(cancellation);
            using var bitmap = await decoder.GetSoftwareBitmapAsync(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Ignore).AsTask(cancellation);
            var result = await engine.RecognizeAsync(bitmap).AsTask(cancellation);
            string text = string.Join("\n", result.Lines.Select(line => line.Text));
            outputBytes = checked(outputBytes + Encoding.UTF8.GetByteCount(text));
            if (outputBytes > request.MaximumOutputBytes)
                throw new DocumentOcrException("OCR_OUTPUT_LIMIT", "OCR output exceeds its budget. / OCR 正文超过额度。");
            recognized.Add(new { page = pageNumber, text });
        }
        return new { protocolVersion = 1, schemaVersion = 1, boundary = "document-ocr", completed = true,
            version = BackendVersion, language = engine.RecognizerLanguage.LanguageTag, pages = recognized };
    }
}
