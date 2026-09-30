using System.Runtime.CompilerServices;
using System.Text;
using System.Text.Json;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

/// <summary>Reads gateway SSE frames independently of network packet and UTF-8 character boundaries.</summary>
public static class ChatStreamReader
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private const int MaximumEventCharacters = 8 * 1024 * 1024;

    public static async IAsyncEnumerable<ChatStreamEvent> ReadAsync(Stream stream,
        Guid conversationId, Guid requestId, [EnumeratorCancellation] CancellationToken cancellationToken = default)
    {
        using var reader = new StreamReader(stream, new UTF8Encoding(false, true),
            detectEncodingFromByteOrderMarks: true, bufferSize: 4096, leaveOpen: true);
        var data = new StringBuilder();
        bool started = false;
        while (true)
        {
            cancellationToken.ThrowIfCancellationRequested();
            string? line = await reader.ReadLineAsync(cancellationToken);
            if (line is null || line.Length == 0)
            {
                if (data.Length > 0)
                {
                    ChatStreamEvent item;
                    try
                    {
                        item = JsonSerializer.Deserialize<ChatStreamEvent>(data.ToString(), JsonOptions)
                            ?? throw new InvalidDataException("模型流返回了空事件。");
                    }
                    catch (JsonException exception)
                    {
                        throw new InvalidDataException("模型流返回了无法识别的数据。", exception);
                    }
                    data.Clear();
                    if (item.ConversationId != conversationId || item.RequestId != requestId || item.CreatedAt == default)
                        throw new InvalidDataException("模型流返回的会话或请求信息不匹配。");
                    if (!started && item.Type != "started")
                        throw new InvalidDataException("模型流缺少开始事件。");
                    bool terminal = false;
                    switch (item.Type)
                    {
                        case "started":
                            if (started) throw new InvalidDataException("模型流重复开始了同一条回复。");
                            started = true;
                            break;
                        case "text_delta":
                        case "reasoning_delta":
                            if (item.Delta is null) throw new InvalidDataException("模型流缺少增量内容。");
                            break;
                        case "completed":
                            if (item.Content is null) throw new InvalidDataException("模型流缺少最终回复。");
                            terminal = true;
                            break;
                        case "interrupted":
                        case "error":
                            terminal = true;
                            break;
                        default:
                            throw new InvalidDataException("模型流返回了未知的事件类型。");
                    }
                    yield return item;
                    if (terminal) yield break;
                }
                if (line is null) throw new EndOfStreamException("模型连接已断开，已保留收到的内容。");
                continue;
            }
            // Ignore comments/heartbeats and optional SSE event/id/retry fields.
            int separator = line.IndexOf(':');
            string field = separator < 0 ? line : line[..separator];
            if (field != "data") continue;
            string value = separator < 0 ? string.Empty : line[(separator + 1)..];
            if (value.StartsWith(' ')) value = value[1..];
            if (data.Length + value.Length + 1 > MaximumEventCharacters)
                throw new InvalidDataException("模型流的单条事件过大。");
            data.Append(value).Append('\n');
        }
    }
}
