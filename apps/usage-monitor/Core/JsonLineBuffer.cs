using System.Text;

namespace Kynxa.UsageMonitor.Core;

/// <summary>
/// Assembles bytes before decoding UTF-8, so split characters and incomplete lines are never parsed early.
/// 先组装完整行的字节再解码，避免跨块 UTF-8 字符或半行被提前解析。
/// </summary>
public sealed class JsonLineBuffer
{
    public const int MaximumLineBytes = 1_048_576;
    private readonly MemoryStream _pending = new();
    private bool _discardUntilNewline;
    private static readonly UTF8Encoding StrictUtf8 = new(false, true);

    public void Reset(bool discardUntilNewline = false)
    {
        _pending.SetLength(0);
        _discardUntilNewline = discardUntilNewline;
    }

    public void Append(ReadOnlySpan<byte> bytes, Action<string> acceptLine, Action<string> warn)
    {
        foreach (byte value in bytes)
        {
            if (_discardUntilNewline)
            {
                if (value == (byte)'\n') _discardUntilNewline = false;
                continue;
            }
            if (value == (byte)'\n')
            {
                try
                {
                    string line = StrictUtf8.GetString(_pending.GetBuffer(), 0, checked((int)_pending.Length)).TrimEnd('\r').TrimStart('\uFEFF');
                    if (!string.IsNullOrWhiteSpace(line)) acceptLine(line);
                }
                catch (DecoderFallbackException)
                {
                    warn("忽略无效 UTF-8 日志行");
                }
                _pending.SetLength(0);
                continue;
            }
            if (_pending.Length >= MaximumLineBytes)
            {
                _pending.SetLength(0);
                _discardUntilNewline = true;
                warn("日志单行超过 1 MiB，已跳过");
                continue;
            }
            _pending.WriteByte(value);
        }
    }
}
