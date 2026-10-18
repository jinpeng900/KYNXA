using System.Text;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;

internal static class GatewayRecoveryStreamChecks
{
    public static async Task RunAsync()
    {
        await VerifyLiveLocalFixtureAsync();
        string? fixturePath = Environment.GetEnvironmentVariable("KYNXA_GATEWAY_RECOVERY_SSE_FIXTURE");
        if (string.IsNullOrWhiteSpace(fixturePath)) return;

        // Replay the Rust gateway's real repair stream; do not synthesize frames or bypass the desktop parser.
        // 重放 Rust 网关实际产生的恢复流，不合成事件，也不绕过桌面正式解析器。
        await using var stream = File.OpenRead(fixturePath);
        var identity = await ReadIdentityAsync(stream);
        stream.Position = 0;
        var events = new List<ChatStreamEvent>();
        await foreach (var streamEvent in ChatStreamReader.ReadAsync(stream, identity.ConversationId, identity.RequestId))
            events.Add(streamEvent);

        if (events.Count == 0 || events[^1].Type != "completed" || events[^1].Content != "你好！")
            throw new InvalidDataException("The real gateway recovery stream did not deliver its expected completed answer.");
        if (!events.Any(streamEvent => streamEvent.Type == "task_progress" && streamEvent.Phase == "recovering"))
            throw new InvalidDataException("The real gateway fixture did not exercise task recovery.");
        if (events.Any(streamEvent => streamEvent.Type is "error" or "interrupted"))
            throw new InvalidDataException("The real gateway recovery stream terminated the conversation instead of continuing.");

        Console.WriteLine($"PASS: real Rust gateway recovery SSE parsed by the desktop reader ({events.Count} events).");
    }

    private static async Task VerifyLiveLocalFixtureAsync()
    {
        string? fixturePath = Environment.GetEnvironmentVariable("KYNXA_LIVE_LOCAL_SSE_FIXTURE");
        if (string.IsNullOrWhiteSpace(fixturePath)) return;

        // Consume the captured local-model gateway stream unchanged through the production parser.
        // 将本地模型网关的实测流原样交给正式解析器；不合成回复，也不预设模型回答内容。
        await using var stream = File.OpenRead(fixturePath);
        var identity = await ReadIdentityAsync(stream);
        stream.Position = 0;
        var events = new List<ChatStreamEvent>();
        await foreach (var streamEvent in ChatStreamReader.ReadAsync(stream, identity.ConversationId, identity.RequestId))
            events.Add(streamEvent);

        // Only an explicitly requested test cancellation may terminate this mode with ABORT_ERR.
        // 只有实测明确主动取消时，才允许此模式以 ABORT_ERR 中断；默认仍要求完整回复。
        bool expectsCancellation = string.Equals(Environment.GetEnvironmentVariable("KYNXA_LIVE_LOCAL_SSE_EXPECT_CANCEL"), "1", StringComparison.Ordinal);
        if (expectsCancellation)
        {
            if (events.Count == 0 || events[^1].Type != "interrupted" || events[^1].Code != "ABORT_ERR")
                throw new InvalidDataException("The live local-model stream did not end with the explicitly requested cancellation.");
        }
        else if (events.Count == 0 || events[^1].Type != "completed" || string.IsNullOrWhiteSpace(events[^1].Content))
            throw new InvalidDataException("The live local-model stream did not finish with a nonempty completed answer.");
        if (events.Any(streamEvent => streamEvent.Type == "error") || events.Take(events.Count - (expectsCancellation ? 1 : 0))
            .Any(streamEvent => streamEvent.Type == "interrupted"))
            throw new InvalidDataException("The live local-model stream ended the conversation instead of continuing.");

        var anchors = events.Where(streamEvent => streamEvent.Type == "reply_anchor").ToArray();
        bool requiresSteering = string.Equals(Environment.GetEnvironmentVariable("KYNXA_LIVE_LOCAL_SSE_EXPECT_STEERING"), "1", StringComparison.Ordinal);
        if (requiresSteering && anchors.Length == 0)
            throw new InvalidDataException("The live local-model stream did not exercise a steering reply boundary.");
        if (anchors.Length > 0 && events[^1].ReplyMessageId != anchors[^1].ReplyMessageId)
            throw new InvalidDataException("The live local-model reply did not remain after the latest steering message.");

        Console.WriteLine($"PASS: live local-model gateway SSE parsed by the desktop reader ({events.Count} events, {anchors.Length} reply anchors, {events.Count(streamEvent => streamEvent.Type == "tool_result")} tool receipts).");
    }

    private static async Task<(Guid ConversationId, Guid RequestId)> ReadIdentityAsync(Stream stream)
    {
        using var reader = new StreamReader(stream, new UTF8Encoding(false, true),
            detectEncodingFromByteOrderMarks: true, bufferSize: 4096, leaveOpen: true);
        var eventData = new StringBuilder();
        while (await reader.ReadLineAsync() is { } line)
        {
            if (line.Length == 0 && eventData.Length > 0) break;
            if (!line.StartsWith("data:", StringComparison.Ordinal)) continue;
            string value = line[5..];
            if (value.StartsWith(' ')) value = value[1..];
            eventData.Append(value).Append('\n');
        }

        var firstEvent = JsonSerializer.Deserialize<ChatStreamEvent>(eventData.ToString(),
            new JsonSerializerOptions(JsonSerializerDefaults.Web));
        if (firstEvent is null || firstEvent.Type != "started" || firstEvent.ConversationId == Guid.Empty || firstEvent.RequestId == Guid.Empty)
            throw new InvalidDataException("The real gateway recovery fixture lacks its initial stream identity.");
        return (firstEvent.ConversationId, firstEvent.RequestId);
    }
}
