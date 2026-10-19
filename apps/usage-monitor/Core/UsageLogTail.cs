using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace Kynxa.UsageMonitor.Core;

public sealed class UsageLogTail
{
    private readonly string _path;
    private readonly bool _replay;
    private readonly JsonLineBuffer _lines = new();
    private bool _initialized;
    private long _offsetBytes;
    private string? _identity;
    private byte[] _prefix = [];
    private byte[] _checkpoint = [];
    private long _checkpointOffset;
    private string? _lastIoError;

    public UsageLogTail(string path, bool replay)
    {
        _path = Path.GetFullPath(path);
        _replay = replay;
    }

    public async Task<int> ReadAvailableAsync(Action<string> acceptLine, Action<string> warn, CancellationToken cancellationToken = default)
    {
        try
        {
            using FileStream stream = new(_path, FileMode.Open, FileAccess.Read,
                FileShare.ReadWrite | FileShare.Delete, 16_384, FileOptions.Asynchronous | FileOptions.SequentialScan);
            _lastIoError = null;
            string identity = FileIdentity(stream);
            byte[] prefix = new byte[Math.Min(64, stream.Length)];
            int prefixRead = await stream.ReadAsync(prefix, cancellationToken);
            if (prefixRead < prefix.Length) Array.Resize(ref prefix, prefixRead);
            bool prefixChanged = _prefix.Length > 0 &&
                (prefix.Length < _prefix.Length || !prefix.AsSpan(0, _prefix.Length).SequenceEqual(_prefix));
            bool checkpointChanged = false;
            if (_checkpoint.Length > 0 && stream.Length >= _checkpointOffset + _checkpoint.Length)
            {
                stream.Position = _checkpointOffset;
                byte[] currentCheckpoint = new byte[_checkpoint.Length];
                int checkpointRead = await stream.ReadAsync(currentCheckpoint, cancellationToken);
                checkpointChanged = checkpointRead != currentCheckpoint.Length || !currentCheckpoint.AsSpan().SequenceEqual(_checkpoint);
            }
            if (_initialized && (_identity != identity || stream.Length < _offsetBytes || prefixChanged || checkpointChanged))
            {
                // A replaced/truncated log starts a new byte stream; deduplication belongs to the accumulator.
                // 文件替换或截断后重置字节流状态，调用去重仍由统计器负责。
                _offsetBytes = 0;
                _checkpoint = [];
                _lines.Reset();
            }
            if (!_initialized)
            {
                _initialized = true;
                _offsetBytes = _replay ? 0 : stream.Length;
                if (_offsetBytes > 0)
                {
                    stream.Position = _offsetBytes - 1;
                    _lines.Reset(stream.ReadByte() != (byte)'\n');
                }
            }
            _identity = identity;
            _prefix = prefix;
            stream.Position = _offsetBytes;
            byte[] buffer = new byte[16_384];
            int totalRead = 0;
            // Bound each pass so backlog cannot monopolize a worker or delay cancellation indefinitely.
            // 每轮最多读取 256 KiB，防止大量历史数据占满后台线程或延迟取消。
            while (totalRead < 262_144)
            {
                int read = await stream.ReadAsync(buffer, cancellationToken);
                if (read == 0) break;
                _offsetBytes += read;
                totalRead += read;
                _lines.Append(buffer.AsSpan(0, read), acceptLine, warn);
            }
            _checkpointOffset = Math.Max(0, _offsetBytes - 64);
            _checkpoint = new byte[_offsetBytes - _checkpointOffset];
            stream.Position = _checkpointOffset;
            int checkpointBytes = await stream.ReadAsync(_checkpoint, cancellationToken);
            if (checkpointBytes != _checkpoint.Length) Array.Resize(ref _checkpoint, checkpointBytes);
            return totalRead;
        }
        catch (FileNotFoundException)
        {
            InitializeAbsentFile();
            return 0;
        }
        catch (DirectoryNotFoundException)
        {
            InitializeAbsentFile();
            return 0;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            string kind = error.GetType().Name;
            if (_lastIoError != kind)
            {
                warn("暂时无法读取用量日志（" + kind + "）");
                _lastIoError = kind;
            }
            return 0;
        }
    }

    private void InitializeAbsentFile()
    {
        // If no log exists at startup, its first creation contains only new events and is read from zero.
        // 启动时不存在日志则等待首次创建，从零读取新事件。
        if (!_initialized)
        {
            _initialized = true;
            _offsetBytes = 0;
        }
    }

    private static string FileIdentity(FileStream stream)
    {
        if (OperatingSystem.IsWindows() && GetFileInformationByHandle(stream.SafeFileHandle, out ByHandleFileInformation information))
        {
            return $"{information.VolumeSerialNumber:x8}:{information.FileIndexHigh:x8}:{information.FileIndexLow:x8}";
        }
        return File.GetCreationTimeUtc(stream.Name).Ticks.ToString(System.Globalization.CultureInfo.InvariantCulture);
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct ByHandleFileInformation
    {
        public uint FileAttributes;
        public System.Runtime.InteropServices.ComTypes.FILETIME CreationTime;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastAccessTime;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWriteTime;
        public uint VolumeSerialNumber;
        public uint FileSizeHigh;
        public uint FileSizeLow;
        public uint NumberOfLinks;
        public uint FileIndexHigh;
        public uint FileIndexLow;
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetFileInformationByHandle(SafeFileHandle fileHandle, out ByHandleFileInformation information);
}
