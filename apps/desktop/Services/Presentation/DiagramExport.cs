using System.Buffers.Binary;
using System.Globalization;
using System.Runtime.InteropServices.WindowsRuntime;
using System.Text;
using System.Text.RegularExpressions;
using System.Xml;
using System.Xml.Linq;
using Windows.Graphics.Imaging;
using Windows.Storage;
using Windows.Storage.Pickers;

namespace KYNXA_Desktop.Services;

public sealed record DiagramExportDocument(string Format, byte[] Bytes, uint Width, uint Height);

public enum DiagramExportOutcome { Success, Cancelled, Error }

/// <summary>
/// Validate self-contained diagram files before opening a native save picker or writing any bytes.
/// 在打开原生保存窗口或写入字节之前，验证图表文件独立且不包含可执行内容。
/// </summary>
public static class DiagramExport
{
    public const int MaximumSvgBytes = 8 * 1024 * 1024;
    public const int MaximumPngBytes = 16 * 1024 * 1024;
    public const uint MaximumDimension = 8192;
    public const uint MaximumSvgDimension = 100000;
    public const long MaximumPixels = 16_000_000;
    private const string SvgNamespace = "http://www.w3.org/2000/svg";
    private const int MaximumSvgNodes = 20000;
    private static readonly UTF8Encoding StrictUtf8 = new(false, true);
    private static int _pickerActive;
    private static long _fileNameSequence;
    private static readonly HashSet<string> SvgElements = new(StringComparer.Ordinal)
    {
        "svg", "g", "defs", "marker", "path", "rect", "circle", "ellipse", "line", "polygon", "polyline",
        "text", "tspan", "title", "desc", "clipPath", "linearGradient", "radialGradient", "stop", "pattern"
    };
    private static readonly HashSet<string> SvgAttributes = new(StringComparer.Ordinal)
    {
        "id", "viewBox", "preserveAspectRatio", "width", "height", "x", "y", "x1", "x2", "y1", "y2", "cx", "cy",
        "r", "rx", "ry", "d", "points", "transform", "fill", "fill-opacity", "fill-rule", "stroke", "stroke-width",
        "stroke-opacity", "stroke-dasharray", "stroke-dashoffset", "stroke-linecap", "stroke-linejoin", "stroke-miterlimit",
        "opacity", "font-family", "font-size", "font-weight", "font-style", "text-anchor", "dominant-baseline",
        "alignment-baseline", "dy", "dx", "textLength", "lengthAdjust", "marker-start", "marker-mid", "marker-end",
        "markerWidth", "markerHeight", "refX", "refY", "orient", "markerUnits", "clip-path", "clipPathUnits",
        "gradientUnits", "gradientTransform", "offset", "stop-color", "stop-opacity", "patternUnits",
        "patternContentUnits", "patternTransform", "role", "aria-label", "aria-labelledby", "aria-describedby", "style"
    };
    private static readonly HashSet<string> StyleProperties = new(StringComparer.Ordinal)
    {
        "fill", "fill-opacity", "fill-rule", "stroke", "stroke-width", "stroke-opacity", "stroke-dasharray",
        "stroke-dashoffset", "stroke-linecap", "stroke-linejoin", "stroke-miterlimit", "opacity", "font-family",
        "font-size", "font-weight", "font-style", "font-variant", "text-anchor", "text-decoration", "dominant-baseline",
        "alignment-baseline", "paint-order", "color", "white-space", "word-spacing", "letter-spacing", "line-height",
        "marker-start", "marker-mid", "marker-end", "clip-path", "rx", "ry", "text-overflow", "overflow", "display",
        "visibility", "stop-color", "stop-opacity"
    };
    private static readonly Regex LocalReference = new("^url\\(\\s*['\"]?#([A-Za-z0-9_.:-]+)['\"]?\\s*\\)$",
        RegexOptions.IgnoreCase | RegexOptions.CultureInvariant, TimeSpan.FromMilliseconds(200));
    private static readonly Regex DangerousValue = new("javascript|vbscript|expression|@import|(?:https?|data|file|ftp)\\s*:|url\\s*\\(",
        RegexOptions.IgnoreCase | RegexOptions.CultureInvariant, TimeSpan.FromMilliseconds(200));
    private static readonly uint[] CrcTable = CreateCrcTable();

    public static async Task<DiagramExportDocument> ValidateAsync(string format, string base64)
    {
        int maximumBytes = format switch
        {
            "svg" => MaximumSvgBytes,
            "png" => MaximumPngBytes,
            _ => throw new InvalidDataException("Unsupported diagram export format.")
        };
        if (string.IsNullOrEmpty(base64) || base64.Length > ((maximumBytes + 2L) / 3) * 4)
            throw new InvalidDataException("Diagram export exceeds the byte limit.");
        byte[] bytes = Convert.FromBase64String(base64);
        if (bytes.Length == 0 || bytes.Length > maximumBytes || Convert.ToBase64String(bytes) != base64)
            throw new InvalidDataException("Diagram export is not canonical base64.");
        return format == "svg" ? ValidateSvg(bytes) : await ValidatePngAsync(bytes);
    }

    private static DiagramExportDocument ValidateSvg(byte[] bytes)
    {
        string text = StrictUtf8.GetString(bytes);
        var settings = new XmlReaderSettings
        {
            DtdProcessing = DtdProcessing.Prohibit,
            XmlResolver = null,
            MaxCharactersInDocument = MaximumSvgBytes,
            MaxCharactersFromEntities = 0
        };
        // Check depth and element count while streaming, before constructing an XML object tree.
        // 先流式检查深度与元素数量，再构造 XML 对象树，避免小文件造成大量节点分配。
        using (var boundsReader = XmlReader.Create(new StringReader(text), settings))
        {
            int nodes = 0;
            while (boundsReader.Read())
                if (boundsReader.Depth > 128 || boundsReader.NodeType == XmlNodeType.Element && ++nodes > MaximumSvgNodes)
                    throw new InvalidDataException("Diagram SVG structure exceeds the limit.");
        }
        using var reader = XmlReader.Create(new StringReader(text), settings);
        var document = XDocument.Load(reader, LoadOptions.PreserveWhitespace);
        var root = document.Root;
        if (root is null || root.Name != XName.Get("svg", SvgNamespace)
            || document.DescendantNodes().Any(node => node is XProcessingInstruction or XDocumentType))
            throw new InvalidDataException("Invalid diagram SVG document.");
        if (document.Declaration?.Encoding is { } encoding && !encoding.Equals("utf-8", StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("Diagram SVG must use UTF-8.");

        var elements = root.DescendantsAndSelf().ToArray();
        var ids = new HashSet<string>(StringComparer.Ordinal);
        foreach (var element in elements)
        {
            if (element.Name.NamespaceName != SvgNamespace || !SvgElements.Contains(element.Name.LocalName))
                throw new InvalidDataException("Unsupported diagram SVG element.");
            if (element.Attribute("id") is { } id && (!ids.Add(id.Value) || id.Value.Length == 0))
                throw new InvalidDataException("Invalid diagram SVG identity.");
        }
        foreach (var element in elements)
        {
            foreach (var attribute in element.Attributes())
            {
                if (attribute.IsNamespaceDeclaration)
                {
                    if (attribute.Name != XName.Get("xmlns") || attribute.Value != SvgNamespace)
                        throw new InvalidDataException("Unsupported diagram SVG namespace.");
                    continue;
                }
                if (attribute.Name.NamespaceName.Length > 0 || !SvgAttributes.Contains(attribute.Name.LocalName)
                    || attribute.Value.Length > 1000000)
                    throw new InvalidDataException("Unsupported diagram SVG attribute.");
                if (attribute.Name.LocalName == "style") ValidateStyle(attribute.Value, ids, element == root);
                else if (attribute.Name.LocalName is not ("id" or "role" or "aria-label" or "aria-labelledby" or "aria-describedby"))
                    ValidateValue(attribute.Value, ids);
            }
        }
        string[] viewBox = Regex.Split(root.Attribute("viewBox")?.Value.Trim() ?? "", "[\\s,]+");
        if (viewBox.Length != 4 || viewBox.Any(value => !double.TryParse(value, NumberStyles.Float, CultureInfo.InvariantCulture, out var number)
            || !double.IsFinite(number) || Math.Abs(number) > MaximumSvgDimension)
            || double.Parse(viewBox[2], CultureInfo.InvariantCulture) <= 0 || double.Parse(viewBox[3], CultureInfo.InvariantCulture) <= 0)
            throw new InvalidDataException("Invalid diagram SVG view box.");
        uint width = ReadDimension(root.Attribute("width")?.Value, MaximumSvgDimension), height = ReadDimension(root.Attribute("height")?.Value, MaximumSvgDimension);
        return new("svg", bytes, width, height);
    }

    private static void ValidateStyle(string style, HashSet<string> ids, bool isRoot)
    {
        foreach (string declaration in style.Split(';', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
        {
            int separator = declaration.IndexOf(':');
            if (separator <= 0) throw new InvalidDataException("Unsupported diagram SVG style.");
            string property = declaration[..separator].Trim(), value = declaration[(separator + 1)..].Trim();
            // Allow fixed responsive sizes only on the root; keep geometry numeric and inert.
            // 仅根文档允许这两个响应式尺寸；图形仍限明确数值，不放宽样式。
            if (isRoot && (property == "width" && value == "100%" || property == "height" && value == "100vh")) continue;
            if (isRoot && property == "background-color" && value.Length == 7 && value[0] == '#' && value.Skip(1).All(Uri.IsHexDigit)) continue;
            if (!StyleProperties.Contains(property))
                throw new InvalidDataException("Unsupported diagram SVG style.");
            ValidateValue(value, ids);
        }
    }

    private static void ValidateValue(string value, HashSet<string> ids)
    {
        if (value.IndexOfAny(['<', '>', '\\', '{', '}', '@']) >= 0 || value.Any(character => char.IsControl(character) && !char.IsWhiteSpace(character)))
            throw new InvalidDataException("Invalid diagram SVG value.");
        if (!DangerousValue.IsMatch(value)) return;
        var match = LocalReference.Match(value);
        if (!match.Success || !ids.Contains(match.Groups[1].Value))
            throw new InvalidDataException("Diagram SVG contains an external or executable reference.");
    }

    private static uint ReadDimension(string? value, uint maximum)
    {
        if (value?.EndsWith("px", StringComparison.Ordinal) == true) value = value[..^2];
        if (!double.TryParse(value, NumberStyles.Float, CultureInfo.InvariantCulture, out var number)
            || !double.IsFinite(number) || number <= 0 || number > maximum)
            throw new InvalidDataException("Invalid diagram export dimensions.");
        return (uint)Math.Ceiling(number);
    }

    private static void ValidateDimensions(uint width, uint height)
    {
        if (width == 0 || height == 0 || width > MaximumDimension || height > MaximumDimension || (long)width * height > MaximumPixels)
            throw new InvalidDataException("Diagram export dimensions exceed the limit.");
    }

    private static async Task<DiagramExportDocument> ValidatePngAsync(byte[] bytes)
    {
        ValidatePngChunks(bytes);
        using var stream = new MemoryStream(bytes, writable: false).AsRandomAccessStream();
        var decoder = await BitmapDecoder.CreateAsync(BitmapDecoder.PngDecoderId, stream);
        ValidateDimensions(decoder.PixelWidth, decoder.PixelHeight);
        // Decode all bounded pixels; a valid signature and header alone cannot prove the PNG is readable.
        // 解码容量受限的全部像素；仅有正确签名和头部并不能证明 PNG 文件可读。
        var pixels = await decoder.GetPixelDataAsync(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Premultiplied,
            new BitmapTransform(), ExifOrientationMode.IgnoreExifOrientation, ColorManagementMode.DoNotColorManage);
        if (pixels.DetachPixelData().LongLength != (long)decoder.PixelWidth * decoder.PixelHeight * 4)
            throw new InvalidDataException("Diagram PNG could not be fully decoded.");
        return new("png", bytes, decoder.PixelWidth, decoder.PixelHeight);
    }

    private static void ValidatePngChunks(byte[] bytes)
    {
        if (!bytes.AsSpan().StartsWith(new byte[] { 137, 80, 78, 71, 13, 10, 26, 10 }))
            throw new InvalidDataException("Invalid diagram PNG signature.");
        int offset = 8;
        bool hasHeader = false, hasImage = false, ended = false;
        while (offset <= bytes.Length - 12)
        {
            uint length = BinaryPrimitives.ReadUInt32BigEndian(bytes.AsSpan(offset, 4));
            if (length > bytes.Length - offset - 12) throw new InvalidDataException("Truncated diagram PNG chunk.");
            int count = (int)length;
            var kindBytes = bytes.AsSpan(offset + 4, 4);
            if (AnyInvalidPngLetter(kindBytes))
                throw new InvalidDataException("Invalid diagram PNG chunk type.");
            string kind = Encoding.ASCII.GetString(kindBytes);
            if (!hasHeader && kind != "IHDR" || hasHeader && kind == "IHDR" || kind is "acTL" or "fcTL" or "fdAT")
                throw new InvalidDataException("Unsupported diagram PNG structure.");
            if (kind == "IHDR")
            {
                if (count != 13) throw new InvalidDataException("Invalid diagram PNG header.");
                ValidateDimensions(BinaryPrimitives.ReadUInt32BigEndian(bytes.AsSpan(offset + 8, 4)),
                    BinaryPrimitives.ReadUInt32BigEndian(bytes.AsSpan(offset + 12, 4)));
                hasHeader = true;
            }
            else if (kind == "IDAT") hasImage = true;
            else if (kind == "IEND")
            {
                if (count != 0 || !hasImage || offset + 12 != bytes.Length)
                    throw new InvalidDataException("Invalid diagram PNG end.");
                ended = true;
            }
            else if (char.IsUpper(kind[0]) && kind != "PLTE") throw new InvalidDataException("Unknown diagram PNG critical chunk.");
            uint crc = uint.MaxValue;
            foreach (byte value in bytes.AsSpan(offset + 4, count + 4)) crc = CrcTable[(crc ^ value) & 255] ^ (crc >> 8);
            if (~crc != BinaryPrimitives.ReadUInt32BigEndian(bytes.AsSpan(offset + 8 + count, 4)))
                throw new InvalidDataException("Invalid diagram PNG checksum.");
            offset += count + 12;
            if (ended) break;
        }
        if (!hasHeader || !ended || offset != bytes.Length) throw new InvalidDataException("Incomplete diagram PNG.");
    }

    private static bool AnyInvalidPngLetter(ReadOnlySpan<byte> value)
    {
        foreach (byte letter in value) if ((letter < 'A' || letter > 'Z') && (letter < 'a' || letter > 'z')) return true;
        return false;
    }

    private static uint[] CreateCrcTable()
    {
        var table = new uint[256];
        for (uint index = 0; index < table.Length; index++)
        {
            uint crc = index;
            for (int bit = 0; bit < 8; bit++) crc = (crc & 1) == 0 ? crc >> 1 : 0xedb88320 ^ (crc >> 1);
            table[index] = crc;
        }
        return table;
    }

    public static async Task<DiagramExportOutcome> SaveAsync(DiagramExportDocument document, nint ownerWindow)
    {
        if (ownerWindow == 0 || Interlocked.CompareExchange(ref _pickerActive, 1, 0) != 0) return DiagramExportOutcome.Error;
        try
        {
            var picker = new FileSavePicker
            {
                SuggestedStartLocation = PickerLocationId.DocumentsLibrary,
                SuggestedFileName = CreateSuggestedFileName(document.Format)
            };
            picker.FileTypeChoices.Add(document.Format.ToUpperInvariant(), ["." + document.Format]);
            WinRT.Interop.InitializeWithWindow.Initialize(picker, ownerWindow);
            var file = await picker.PickSaveFileAsync();
            if (file is null) return DiagramExportOutcome.Cancelled;
            await WriteAsync(file, document);
            return DiagramExportOutcome.Success;
        }
        catch (OperationCanceledException)
        {
            return DiagramExportOutcome.Cancelled;
        }
        catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException or ArgumentException or InvalidOperationException
            or NotSupportedException or System.Runtime.InteropServices.COMException)
        {
            return DiagramExportOutcome.Error;
        }
        finally { Volatile.Write(ref _pickerActive, 0); }
    }

    public static async Task WriteAsync(StorageFile file, DiagramExportDocument document)
    {
        if (document.Format is not ("svg" or "png") || !file.FileType.Equals("." + document.Format, StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("The selected extension does not match the diagram format.");
        await FileIO.WriteBytesAsync(file, document.Bytes);
    }

    public static string CreateSuggestedFileName(string format)
    {
        if (format is not ("svg" or "png")) throw new ArgumentException("Unsupported diagram export format.", nameof(format));
        // Use a safe timestamp and process sequence to distinguish same-millisecond exports.
        // 用安全时间戳和进程序号区分同毫秒导出，不读取聊天文本或路径。
        return "KYNXA-diagram-" + format + "-" + DateTime.Now.ToString("yyyyMMdd-HHmmss-fff", CultureInfo.InvariantCulture)
            + "-" + Interlocked.Increment(ref _fileNameSequence).ToString("x4", CultureInfo.InvariantCulture);
    }
}
