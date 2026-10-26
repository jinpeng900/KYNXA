using System.Text;
using System.Text.Json;
using SharpCompress.Archives;
using SharpCompress.Common;
using SharpCompress.Readers;

namespace KYNXA.ToolHost;

internal sealed record DocumentArchiveRequest
{
    public string Operation { get; init; } = "document_archive";
    public string BytesBase64 { get; init; } = "";
    public string Format { get; init; } = "";
    public string Name { get; init; } = "";
    public long MaximumEntryBytes { get; init; } = 32L * 1024 * 1024;
    public long MaximumExpandedBytes { get; init; } = 512L * 1024 * 1024;
    public int MaximumEntries { get; init; } = 20000;
    public int MaximumRatio { get; init; } = 1000;
}

internal sealed class DocumentArchiveException(string code, string message) : Exception(message)
{
    public string Code { get; } = code;
}

/// <summary>
/// Decode authorized immutable bytes in an owned worker, without extraction or execution.
/// 在受控子进程解码已授权的固定字节，不落盘解压或执行内容。
/// </summary>
internal static class DocumentArchive
{
    internal const int MaximumInputBytes = 32 * 1024 * 1024;
    internal const int MaximumFrameCharacters = 46 * 1024 * 1024;
    private const string Version = "sharpcompress-0.50.5-archive-v1";

    internal static async Task RunAsync(DocumentArchiveRequest request, CancellationToken cancellation)
    {
        if (request.Operation != "document_archive" || request.BytesBase64.Length is < 1 or > MaximumFrameCharacters ||
            request.MaximumEntryBytes is < 1 or > MaximumInputBytes ||
            request.MaximumExpandedBytes is < 1 or > 2L * 1024 * 1024 * 1024 ||
            request.MaximumEntries is < 1 or > 100000 || request.MaximumRatio is < 1 or > 10000 ||
            request.Name.Length is < 1 or > 4096)
            throw Failure("ARCHIVE_INVALID_REQUEST", "Invalid archive decoding budget. / 压缩包解码额度无效。");
        byte[] bytes;
        try { bytes = Convert.FromBase64String(request.BytesBase64); }
        catch (FormatException) { throw Failure("ARCHIVE_INVALID_REQUEST", "Invalid archive bytes. / 压缩包字节无效。"); }
        if (bytes.Length > MaximumInputBytes || !SignatureMatches(bytes, request.Format))
            throw Failure("ARCHIVE_FORMAT_MISMATCH", "Archive bytes do not match their format. / 压缩包字节与格式不符。");
        cancellation.ThrowIfCancellationRequested();
        using var input = new MemoryStream(bytes, writable: false);
        // Read archive entries as streams, with cancellation and per-entry byte limits.
        // 仅流式读取归档成员，不落盘解压；按实际字节限量并支持取消。

        await EmitAsync(new { kind = "header", protocolVersion = 1, boundary = "document-archive", version = Version }, cancellation);
        long expandedBytes = 0;
        int entries = 0;
        var names = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (ArchiveMember entry in Members(input, request, cancellation))
        {
            cancellation.ThrowIfCancellationRequested();
            if (++entries > request.MaximumEntries)
                throw Failure("ARCHIVE_ENTRY_LIMIT", "Archive entry count exceeds the budget. / 压缩包成员数超过额度。");
            string name = string.IsNullOrWhiteSpace(entry.Key) ? DecompressedName(request.Name) : entry.Key;
            if (!TryMemberName(name, out string member))
            {
                await EmitAsync(new { kind = "failure", member = name, code = "ARCHIVE_UNSAFE_MEMBER" }, cancellation);
                continue;
            }
            if (entry.IsDirectory) continue;
            if (!names.Add(member))
            {
                await EmitAsync(new { kind = "failure", member, code = "ARCHIVE_DUPLICATE_MEMBER" }, cancellation);
                continue;
            }
            if (entry.IsEncrypted)
            {
                await EmitAsync(new { kind = "failure", member, code = "ARCHIVE_ENCRYPTED" }, cancellation);
                continue;
            }
            if (!string.IsNullOrEmpty(entry.LinkTarget) || entry.Attrib is int attributes &&
                ((attributes & 0x400) != 0 || ((attributes >> 16) & 0xf000) is 0xa000 or 0x6000 or 0x2000))
            {
                await EmitAsync(new { kind = "failure", member, code = "ARCHIVE_LINK_MEMBER" }, cancellation);
                continue;
            }
            if (entry.IsSplitAfter || entry.VolumeIndexFirst != entry.VolumeIndexLast)
                throw Failure("ARCHIVE_VOLUME_REQUIRED", "All archive volumes are required. / 需要完整的分卷压缩包。");
            if (entry.Size < 0 || entry.Size > request.MaximumEntryBytes ||
                entry.Size > request.MaximumExpandedBytes - expandedBytes)
                throw Failure("ARCHIVE_EXPANDED_LIMIT", "Archive member exceeds the expanded byte budget. / 成员超过展开字节额度。");
            using Stream decoded = entry.OpenStream();
            using var output = new MemoryStream();
            byte[] buffer = new byte[64 * 1024];
            int read;
            while ((read = await decoded.ReadAsync(buffer, cancellation)) != 0)
            {
                cancellation.ThrowIfCancellationRequested();
                long nextMemberBytes = output.Length + read;
                if (nextMemberBytes > request.MaximumEntryBytes || read > request.MaximumExpandedBytes - expandedBytes ||
                    expandedBytes + read > Math.Max(1024 * 1024L, bytes.LongLength * request.MaximumRatio))
                    throw Failure("ARCHIVE_EXPANDED_LIMIT", "Archive expansion exceeds its byte or ratio budget. / 展开超过字节或比例额度。");
                expandedBytes += read;
                await output.WriteAsync(buffer.AsMemory(0, read), cancellation);
            }
            if (entry.Size > 0 && output.Length != entry.Size)
                throw Failure("ARCHIVE_MEMBER_CORRUPT", "Decoded member size differs from its header. / 成员实际大小与头部不符。");
            await EmitAsync(new { kind = "member", member, rawBytes = output.Length,
                bytesBase64 = Convert.ToBase64String(output.GetBuffer(), 0, checked((int)output.Length)) }, cancellation);
        }
        await EmitAsync(new { kind = "complete", protocolVersion = 1, entries, expandedBytes, version = Version }, cancellation);
    }

    private sealed record ArchiveMember(string? Key, long Size, bool IsDirectory, bool IsEncrypted,
        string? LinkTarget, int? Attrib, bool IsSplitAfter, int VolumeIndexFirst, int VolumeIndexLast, Func<Stream> OpenStream);

    private static ArchiveMember Member(IEntry entry, Func<Stream> open) => new(entry.Key, entry.Size, entry.IsDirectory,
        entry.IsEncrypted, entry.LinkTarget, entry.Attrib, entry.IsSplitAfter, entry.VolumeIndexFirst, entry.VolumeIndexLast, open);

    private static Stream DecodeWrapper(Stream input, string format)
    {
        if (format is "lzma" or "tar.lzma")
        {
            byte[] properties = new byte[5]; input.ReadExactly(properties);
            byte[] sizeBytes = new byte[8]; input.ReadExactly(sizeBytes);
            long outputSize = System.Buffers.Binary.BinaryPrimitives.ReadInt64LittleEndian(sizeBytes);
            if (outputSize < -1) throw Failure("ARCHIVE_INVALID_INPUT", "Invalid LZMA size. / LZMA 大小无效。");
            return SharpCompress.Compressors.LZMA.LzmaStream.Create(properties, input,
                input.CanSeek ? input.Length - input.Position : -1, outputSize, leaveOpen: true);
        }
        CompressionType compression = format switch
        {
            "xz" or "tar.xz" => CompressionType.Xz,
            "lzip" or "tar.lz" => CompressionType.LZip,
            "zstd" or "tar.zst" => CompressionType.ZStandard,
            _ => CompressionType.Lzw
        };
        return SharpCompress.Providers.CompressionProviderRegistry.Default.CreateDecompressStream(compression, input);
    }

    private static IEnumerable<ArchiveMember> Members(Stream input, DocumentArchiveRequest request, CancellationToken cancellation)
    {
        string format = request.Format;
        var options = new ReaderOptions { LeaveStreamOpen = true, LookForHeader = false,
            ExtensionHint = format, DisableCheckIncomplete = false };
        if (format == "7z")
        {
            using var archive = ArchiveFactory.OpenArchive(input, options);
            foreach (IArchiveEntry entry in archive.Entries) yield return Member(entry, entry.OpenEntryStream);
        }
        else if (format is "xz" or "tar.xz" or "lzip" or "tar.lz" or "zstd" or "tar.zst" or "compress" or "lzma" or "tar.lzma")
        {
            using Stream decoded = DecodeWrapper(input, format);
            if (format.StartsWith("tar.", StringComparison.Ordinal))
            {
                // Platform TAR streaming avoids codec-factory rewinds while accounting for skipped member bytes.
                // 平台 TAR 流读取器不经过压缩工厂回绕；被跳过的成员字节仍计入总量和展开比例。
                long expandedBudget = Math.Min(request.MaximumExpandedBytes,
                    Math.Max(1024 * 1024L, input.Length * request.MaximumRatio));
                using var bounded = new BoundedDecodedStream(decoded, expandedBudget, cancellation);
                using var reader = new System.Formats.Tar.TarReader(bounded, leaveOpen: true);
                System.Formats.Tar.TarEntry? entry;
                while ((entry = reader.GetNextEntry(copyData: false)) is not null)
                {
                    bool directory = entry.EntryType == System.Formats.Tar.TarEntryType.Directory;
                    bool regular = entry.EntryType is System.Formats.Tar.TarEntryType.RegularFile
                        or System.Formats.Tar.TarEntryType.V7RegularFile or System.Formats.Tar.TarEntryType.ContiguousFile;
                    yield return new ArchiveMember(entry.Name, entry.Length, directory, false,
                        regular || directory ? null : "non-regular-tar-member", null, false, 0, 0,
                        () => entry.DataStream ?? Stream.Null);
                }
            }
            else yield return new ArchiveMember(null, 0, false, false, null, null, false, 0, 0, () => decoded);
        }
        else
        {
            using var reader = ReaderFactory.OpenReader(input, options);
            while (reader.MoveToNextEntry()) yield return Member(reader.Entry, reader.OpenEntryStream);
        }
    }

    private sealed class BoundedDecodedStream(Stream inner, long remainingBytes, CancellationToken cancellation) : Stream
    {
        public override bool CanRead => true;
        public override bool CanSeek => false;
        public override bool CanWrite => false;
        public override long Length => throw new NotSupportedException();
        public override long Position { get => throw new NotSupportedException(); set => throw new NotSupportedException(); }
        public override int Read(byte[] buffer, int offset, int count)
        {
            cancellation.ThrowIfCancellationRequested();
            if (remainingBytes <= 0) throw Failure("ARCHIVE_EXPANDED_LIMIT", "Decoded TAR stream exceeds its budget. / TAR 展开流超过额度。");
            int read = inner.Read(buffer, offset, (int)Math.Min(count, remainingBytes));
            remainingBytes -= read;
            return read;
        }
        public override void Flush() { }
        public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException();
        public override void SetLength(long value) => throw new NotSupportedException();
        public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException();
    }

    internal static string ErrorCode(Exception exception) => exception switch
    {
        DocumentArchiveException error => error.Code,
        OperationCanceledException => "ARCHIVE_CANCELLED",
        SharpCompress.Common.CryptographicException => "ARCHIVE_ENCRYPTED",
        IncompleteArchiveException => "ARCHIVE_VOLUME_REQUIRED",
        InvalidFormatException => "ARCHIVE_INVALID_INPUT",
        _ when exception.Message.Contains("password", StringComparison.OrdinalIgnoreCase) => "ARCHIVE_ENCRYPTED",
        _ => "ARCHIVE_PARSE_FAILED"
    };

    private static DocumentArchiveException Failure(string code, string message) => new(code, message);

    private static async Task EmitAsync(object value, CancellationToken cancellation)
    {
        string frame = JsonSerializer.Serialize(value, Program.JsonOptions);
        await Console.Out.WriteLineAsync(frame.AsMemory(), cancellation);
        await Console.Out.FlushAsync(cancellation);
    }

    private static string DecompressedName(string name)
    {
        int period = name.LastIndexOf('.');
        return period > 0 ? name[..period] : "content.txt";
    }

    private static bool TryMemberName(string name, out string member)
    {
        member = name.Replace('\\', '/');
        if (member.Length is < 1 or > 4096 || member.StartsWith('/') ||
            member.Any(character => character < 32 || character is ':' or '\0' or '\u007f')) return false;
        string[] segments = member.TrimEnd('/').Split('/');
        if (segments.Any(segment => segment.Length == 0 || segment is "." or ".." || segment.EndsWith('.') || segment.EndsWith(' '))) return false;
        member = string.Join('/', segments);
        return true;
    }

    private static bool SignatureMatches(ReadOnlySpan<byte> bytes, string format) => format switch
    {
        "rar" => bytes.StartsWith("Rar!\x1a\x07"u8),
        "7z" => bytes.StartsWith(new byte[] { 0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c }),
        "xz" or "tar.xz" => bytes.StartsWith(new byte[] { 0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00 }),
        "zstd" or "tar.zst" => bytes.StartsWith(new byte[] { 0x28, 0xb5, 0x2f, 0xfd }),
        "lzip" or "tar.lz" => bytes.StartsWith("LZIP"u8),
        "compress" => bytes.StartsWith(new byte[] { 0x1f, 0x9d }),
        "arj" => bytes.StartsWith(new byte[] { 0x60, 0xea }),
        "arc" => bytes.StartsWith(new byte[] { 0x1a }),
        "ace" => bytes.Length >= 14 && bytes.Slice(7, 7).SequenceEqual("**ACE**"u8),
        "lzma" or "tar.lzma" => bytes.Length >= 13 && bytes[0] <= 224,
        _ => false
    };
}
