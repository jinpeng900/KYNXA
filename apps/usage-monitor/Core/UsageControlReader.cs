using System.Text;
using System.Text.Json;

namespace Kynxa.UsageMonitor.Core;

/// <summary>
/// Reads only bounded snapshot commands from the inherited owner pipe; malformed data never becomes UI state.
/// 仅接收宿主继承管道中的有界快照命令，损坏数据不进入界面状态。
/// </summary>
public sealed class UsageControlReader
{
    public const int MaximumCommandBytes = 65_536;
    public const string ReadyCommand = "KYNXA_USAGE_MONITOR_READY_V1";
    public const string ErrorCommand = "KYNXA_USAGE_MONITOR_ERROR_V1";
    public const string CloseCommand = "KYNXA_USAGE_MONITOR_CLOSE_V1";
    private static readonly UTF8Encoding StrictUtf8 = new(false, true);
    private static readonly JsonSerializerOptions JsonOptions = new() { PropertyNameCaseInsensitive = true };
    private readonly MemoryStream _pending = new();
    private bool _discardUntilNewline;

    public void Append(ReadOnlySpan<byte> bytes, Action<UsageDisplayState> acceptSnapshot, Action close, Action invalidCommand)
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
                ReadCommand(acceptSnapshot, close, invalidCommand);
                _pending.SetLength(0);
            }
            else if (_pending.Length >= MaximumCommandBytes)
            {
                _pending.SetLength(0);
                _discardUntilNewline = true;
                invalidCommand();
            }
            else
            {
                _pending.WriteByte(value);
            }
        }
    }

    private void ReadCommand(Action<UsageDisplayState> acceptSnapshot, Action close, Action invalidCommand)
    {
        try
        {
            string command = StrictUtf8.GetString(_pending.GetBuffer(), 0, checked((int)_pending.Length)).TrimEnd('\r');
            if (command == CloseCommand)
            {
                close();
                return;
            }
            using JsonDocument document = JsonDocument.Parse(command, new JsonDocumentOptions { MaxDepth = 8 });
            JsonElement root = document.RootElement;
            if (root.ValueKind != JsonValueKind.Object || !root.TryGetProperty("event", out JsonElement eventValue) ||
                eventValue.ValueKind != JsonValueKind.String || eventValue.GetString() != "snapshot" ||
                !root.TryGetProperty("snapshot", out JsonElement value) || value.ValueKind != JsonValueKind.Object)
            {
                invalidCommand();
                return;
            }
            UsageDisplayState? snapshot = value.Deserialize<UsageDisplayState>(JsonOptions);
            if (snapshot is null || !IsValid(snapshot))
            {
                invalidCommand();
                return;
            }
            acceptSnapshot(snapshot);
        }
        catch (Exception error) when (error is JsonException or DecoderFallbackException)
        {
            invalidCommand();
        }
    }

    private static bool IsValid(UsageDisplayState state)
    {
        string?[] labels = [state.TotalText, state.CacheText, state.InputText, state.OutputText, state.CacheHitText,
            state.ScopeLabel, state.TotalLabel, state.CacheLabel, state.CollapseLabel, state.ExpandLabel,
            state.ResetPositionLabel, state.TopmostLabel, state.CloseLabel];
        return labels.All(value => value is not null && value.Length <= 256 && !value.Any(char.IsControl)) &&
            state.Description is not null && state.Description.Length <= 8_192 &&
            IsColor(state.Accent) && IsColor(state.Surface) && IsColor(state.Text) && IsColor(state.Secondary) && IsColor(state.Border);
    }

    private static bool IsColor(string? value)
    {
        return value is { Length: 7 } && value[0] == '#' && value.AsSpan(1).IndexOfAnyExcept("0123456789abcdefABCDEF") < 0;
    }
}
