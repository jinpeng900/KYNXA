using System.Text.Json;
using System.Text.Json.Serialization;
using System.Diagnostics;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Models.UI;

[JsonConverter(typeof(ChatMessageStateConverter))]
public sealed class ChatMessageState
{
    public Guid Id { get; set; } = Guid.NewGuid();
    public string Role { get; set; } = "user";
    public string Content { get; set; } = string.Empty;
    public List<ChatAttachment> Attachments { get; set; } = [];
    public string Reasoning { get; set; } = string.Empty;
    private string _status = "completed";
    public string Status
    {
        get => _status;
        set
        {
            if (_status == "streaming" && value != "streaming") FreezeGenerationTiming();
            _status = value;
        }
    }
    public string Error { get; set; } = string.Empty;
    // Steering freezes prior output above the guidance and links the next reply to that guidance.
    // 引导前的输出停留在上方，新回复关联引导消息；历史正文保持可见。
    public Guid? SupersededByUserMessageId { get; set; }
    public Guid? InReplyToUserMessageId { get; set; }
    public Guid? OriginUserMessageId { get; set; }
    // Display task outcomes separately from reply transport status.
    // 分开展示任务结果和传输状态；界面投影不能决定正式结果。
    [JsonIgnore]
    public string TaskCompletionState { get; set; } = string.Empty;
    [JsonIgnore]
    public string ExecutionPhase { get; set; } = string.Empty;

    public static string ReadTaskCompletion(JsonElement? value)
    {
        if (value is not { ValueKind: JsonValueKind.Object } completion
            || !completion.TryGetProperty("completionState", out var state) || state.ValueKind != JsonValueKind.String) return "";
        return state.GetString() is "completed" or "partial" or "verification-failed" or "execution-unknown" ? state.GetString()! : "";
    }
    public string Provider { get; set; } = string.Empty;
    public string Model { get; set; } = string.Empty;
    public long ReasoningDurationMs { get; set; }
    public long DurationMs { get; set; }
    // The live UI clock is process-local; persisted totals remain owned by the gateway.
    // 活动计时仅存在于当前进程；正式总用时仍由网关保存，不持久化 Stopwatch 时间戳。
    [JsonIgnore]
    public long? GenerationStartedTimestamp { get; set; }
    [JsonIgnore]
    public Guid GenerationClockId { get; private set; } = Guid.NewGuid();
    private long _generationElapsedBaseMs;
    private bool _generationTimingKnown;

    [JsonIgnore]
    public long? GenerationElapsedMs
    {
        get
        {
            if (Status != "streaming" && ChatDurationRules.IsValid(DurationMs) && DurationMs > 0) return DurationMs;
            if (GenerationStartedTimestamp is not long started) return _generationTimingKnown ? _generationElapsedBaseMs : null;
            long elapsed = (long)Math.Clamp(Stopwatch.GetElapsedTime(started).TotalMilliseconds, 0, ChatDurationRules.MaximumDurationMs);
            return _generationElapsedBaseMs + Math.Min(elapsed, ChatDurationRules.MaximumDurationMs - _generationElapsedBaseMs);
        }
    }
    [JsonIgnore]
    public string GenerationTimingState => GenerationElapsedMs is null ? "unknown"
        : Status != "streaming" ? "settled" : GenerationStartedTimestamp is not null ? "live" : "paused";

    /// <summary>Start after generation preparation, or restore the gateway's known cumulative timing.</summary>
    /// <remarks>生成准备完成后启表；恢复时沿用网关已知累计值，准备与暂停期间不偷偷累计。</remarks>
    public void StartGenerationTiming()
    {
        _generationTimingKnown = true;
        GenerationStartedTimestamp ??= Stopwatch.GetTimestamp();
    }

    // Restore the gateway's cumulative clock independently of visible reply segments.
    // 网关累计计时独立于可见回复分段；恢复等待时保持已知值，不把离线时间算进去。
    public void ApplyGenerationTiming(long elapsedMs, bool active)
    {
        if (!ChatDurationRules.IsValid(elapsedMs)) return;
        long current = GenerationElapsedMs ?? 0;
        _generationElapsedBaseMs = active && GenerationStartedTimestamp is not null ? Math.Max(current, elapsedMs) : elapsedMs;
        _generationTimingKnown = true;
        GenerationStartedTimestamp = active ? Stopwatch.GetTimestamp() : null;
    }

    private void FreezeGenerationTiming()
    {
        if (GenerationElapsedMs is not long elapsed) return;
        _generationElapsedBaseMs = elapsed;
        _generationTimingKnown = true;
        GenerationStartedTimestamp = null;
    }

    // Steering changes the visible reply anchor while retaining the same task clock.
    // 中途引导只换回复锚点，沿用任务时钟；新回复使用新的时钟标识。
    public void InheritGenerationTiming(ChatMessageState source)
    {
        if (source.GenerationElapsedMs is not long elapsed) return;
        if (GenerationElapsedMs is long existing && existing > elapsed)
        {
            GenerationClockId = source.GenerationClockId;
            return;
        }
        _generationElapsedBaseMs = source.GenerationStartedTimestamp is not null ? source._generationElapsedBaseMs : elapsed;
        _generationTimingKnown = true;
        GenerationStartedTimestamp = source.GenerationStartedTimestamp;
        GenerationClockId = source.GenerationClockId;
    }
    public List<ToolActivity> ToolActivities { get; set; } = [];
    public List<AssistantSegment> AssistantSegments { get; set; } = [];
    public DateTimeOffset CreatedAt { get; set; } = DateTimeOffset.UtcNow;
}

/// <summary>
/// Preserve legacy plain-string user messages when loading transcripts.
/// 加载时兼容旧版预览保存的纯字符串用户消息。
/// </summary>
public sealed class ChatMessageStateConverter : JsonConverter<ChatMessageState>
{
    private static readonly JsonSerializerOptions ToolOptions = new(JsonSerializerDefaults.Web);
    public override ChatMessageState Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
    {
        if (reader.TokenType == JsonTokenType.String) return new ChatMessageState { Content = reader.GetString() ?? string.Empty };
        using var document = JsonDocument.ParseValue(ref reader);
        var value = document.RootElement;
        string status = ReadString(value, "Status", "completed");
        return new ChatMessageState
        {
            Id = value.TryGetProperty("Id", out var id) ? id.GetGuid() : Guid.NewGuid(),
            Role = value.GetProperty("Role").GetString() ?? "user",
            Content = value.GetProperty("Content").GetString() ?? string.Empty,
            Attachments = value.TryGetProperty("Attachments", out var attachments) || value.TryGetProperty("attachments", out attachments)
                ? attachments.Deserialize<List<ChatAttachment>>(ToolOptions) ?? [] : [],
            Reasoning = ReadString(value, "Reasoning"),
            // A process that exited during generation cannot resume the old HTTP stream.
            // 生成期间进程退出后，无法恢复旧 HTTP 流。
            Status = status == "streaming" ? "interrupted" : status,
            Error = ReadString(value, "Error"),
            SupersededByUserMessageId = ReadMessageId(value, "SupersededByUserMessageId"),
            InReplyToUserMessageId = ReadMessageId(value, "InReplyToUserMessageId"),
            OriginUserMessageId = ReadMessageId(value, "OriginUserMessageId"),
            TaskCompletionState = ReadStoredTaskCompletion(value),
            Provider = ReadString(value, "Provider"),
            Model = ReadString(value, "Model"),
            ToolActivities = value.TryGetProperty("ToolActivities", out var tools) || value.TryGetProperty("toolActivities", out tools)
                ? tools.Deserialize<List<ToolActivity>>(ToolOptions) ?? [] : [],
            AssistantSegments = value.TryGetProperty("AssistantSegments", out var segments) || value.TryGetProperty("assistantSegments", out segments)
                ? ReadSegments(segments, status) : [],
            ReasoningDurationMs = value.TryGetProperty("ReasoningDurationMs", out var duration) &&
                duration.TryGetInt64(out long milliseconds) ? Math.Max(0, milliseconds) : 0,
            DurationMs = ReadDuration(value),
            CreatedAt = value.TryGetProperty("CreatedAt", out var createdAt) ? createdAt.GetDateTimeOffset() : DateTimeOffset.UtcNow
        };
    }

    public override void Write(Utf8JsonWriter writer, ChatMessageState value, JsonSerializerOptions options)
    {
        writer.WriteStartObject();
        writer.WriteString("Id", value.Id);
        writer.WriteString("Role", value.Role);
        writer.WriteString("Content", value.Content);
        if (value.Attachments.Count > 0)
        {
            writer.WritePropertyName("Attachments");
            JsonSerializer.Serialize(writer, value.Attachments, ToolOptions);
        }
        writer.WriteString("Reasoning", value.Reasoning);
        writer.WriteString("Status", value.Status);
        writer.WriteString("Error", value.Error);
        if (value.SupersededByUserMessageId is { } supersededBy) writer.WriteString("SupersededByUserMessageId", supersededBy);
        if (value.InReplyToUserMessageId is { } inReplyTo) writer.WriteString("InReplyToUserMessageId", inReplyTo);
        if (value.OriginUserMessageId is { } origin) writer.WriteString("OriginUserMessageId", origin);
        writer.WriteString("Provider", value.Provider);
        writer.WriteString("Model", value.Model);
        writer.WriteNumber("ReasoningDurationMs", value.ReasoningDurationMs);
        if (value.DurationMs is < 0 or > 9007199254740991) throw new JsonException("Invalid reply duration.");
        writer.WriteNumber("DurationMs", value.DurationMs);
        writer.WritePropertyName("ToolActivities");
        JsonSerializer.Serialize(writer, value.ToolActivities, ToolOptions);
        writer.WritePropertyName("AssistantSegments");
        JsonSerializer.Serialize(writer, value.AssistantSegments, ToolOptions);
        writer.WriteString("CreatedAt", value.CreatedAt);
        writer.WriteEndObject();
    }

    private static string ReadString(JsonElement value, string name, string fallback = "") =>
        value.TryGetProperty(name, out var field) ? field.GetString() ?? fallback : fallback;

    private static Guid? ReadMessageId(JsonElement value, string name) =>
        value.TryGetProperty(name, out var field) && field.ValueKind != JsonValueKind.Null ? field.GetGuid() : null;

    private static string ReadStoredTaskCompletion(JsonElement value)
    {
        if (!(value.TryGetProperty("ToolRun", out var run) || value.TryGetProperty("toolRun", out run))
            || run.ValueKind != JsonValueKind.Object || !run.TryGetProperty("taskCompletion", out var completion)) return "";
        return ChatMessageState.ReadTaskCompletion(completion);
    }

    private static long ReadDuration(JsonElement value)
    {
        if (!value.TryGetProperty("DurationMs", out var duration) && !value.TryGetProperty("durationMs", out duration) || duration.ValueKind == JsonValueKind.Null)
            return 0;
        if (duration.ValueKind != JsonValueKind.Number || !duration.TryGetInt64(out long milliseconds) || milliseconds is < 0 or > 9007199254740991)
            throw new JsonException("Invalid reply duration.");
        return milliseconds;
    }

    private static List<AssistantSegment> ReadSegments(JsonElement value, string messageStatus)
    {
        var segments = value.Deserialize<List<AssistantSegment>>(ToolOptions) ?? [];
        if (!AssistantSegmentRules.IsValidSequence(segments)) throw new JsonException("Invalid assistant segments.");
        // A stopped process has no live stream even when its last durable round was still generating.
        // 停止的进程没有活动流，即使最后保存的一轮仍处于生成状态。
        return messageStatus == "streaming"
            ? segments.Select(segment => segment.Status == "streaming" ? segment with { Status = "interrupted" } : segment).ToList()
            : segments;
    }
}
