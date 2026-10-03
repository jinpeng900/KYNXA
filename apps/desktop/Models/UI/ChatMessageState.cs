using System.Text.Json;
using System.Text.Json.Serialization;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Models.UI;

[JsonConverter(typeof(ChatMessageStateConverter))]
public sealed class ChatMessageState
{
    public Guid Id { get; set; } = Guid.NewGuid();
    public string Role { get; set; } = "user";
    public string Content { get; set; } = string.Empty;
    public string Reasoning { get; set; } = string.Empty;
    public string Status { get; set; } = "completed";
    public string Error { get; set; } = string.Empty;
    public string Provider { get; set; } = string.Empty;
    public string Model { get; set; } = string.Empty;
    public long ReasoningDurationMs { get; set; }
    public List<ToolActivity> ToolActivities { get; set; } = [];
    public DateTimeOffset CreatedAt { get; set; } = DateTimeOffset.UtcNow;
}

/// <summary>Older previews stored each user message as a plain string. Preserve them when loading new transcripts.</summary>
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
            Reasoning = ReadString(value, "Reasoning"),
            // A process that exited during generation cannot resume the old HTTP stream.
            Status = status == "streaming" ? "interrupted" : status,
            Error = ReadString(value, "Error"),
            Provider = ReadString(value, "Provider"),
            Model = ReadString(value, "Model"),
            ToolActivities = value.TryGetProperty("ToolActivities", out var tools) || value.TryGetProperty("toolActivities", out tools)
                ? tools.Deserialize<List<ToolActivity>>(ToolOptions) ?? [] : [],
            ReasoningDurationMs = value.TryGetProperty("ReasoningDurationMs", out var duration) &&
                duration.TryGetInt64(out long milliseconds) ? Math.Max(0, milliseconds) : 0,
            CreatedAt = value.TryGetProperty("CreatedAt", out var createdAt) ? createdAt.GetDateTimeOffset() : DateTimeOffset.UtcNow
        };
    }

    public override void Write(Utf8JsonWriter writer, ChatMessageState value, JsonSerializerOptions options)
    {
        writer.WriteStartObject();
        writer.WriteString("Id", value.Id);
        writer.WriteString("Role", value.Role);
        writer.WriteString("Content", value.Content);
        writer.WriteString("Reasoning", value.Reasoning);
        writer.WriteString("Status", value.Status);
        writer.WriteString("Error", value.Error);
        writer.WriteString("Provider", value.Provider);
        writer.WriteString("Model", value.Model);
        writer.WriteNumber("ReasoningDurationMs", value.ReasoningDurationMs);
        writer.WritePropertyName("ToolActivities");
        JsonSerializer.Serialize(writer, value.ToolActivities, ToolOptions);
        writer.WriteString("CreatedAt", value.CreatedAt);
        writer.WriteEndObject();
    }

    private static string ReadString(JsonElement value, string name, string fallback = "") =>
        value.TryGetProperty(name, out var field) ? field.GetString() ?? fallback : fallback;
}
