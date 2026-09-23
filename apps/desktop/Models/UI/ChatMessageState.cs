using System.Text.Json;
using System.Text.Json.Serialization;

namespace KYNXA_Desktop.Models.UI;

[JsonConverter(typeof(ChatMessageStateConverter))]
public sealed class ChatMessageState
{
    public Guid Id { get; set; } = Guid.NewGuid();
    public string Role { get; set; } = "user";
    public string Content { get; set; } = string.Empty;
    public DateTimeOffset CreatedAt { get; set; } = DateTimeOffset.UtcNow;
}

/// <summary>Older previews stored each user message as a plain string. Preserve them when loading new transcripts.</summary>
public sealed class ChatMessageStateConverter : JsonConverter<ChatMessageState>
{
    public override ChatMessageState Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
    {
        if (reader.TokenType == JsonTokenType.String) return new ChatMessageState { Content = reader.GetString() ?? string.Empty };
        using var document = JsonDocument.ParseValue(ref reader);
        var value = document.RootElement;
        return new ChatMessageState
        {
            Id = value.TryGetProperty("Id", out var id) ? id.GetGuid() : Guid.NewGuid(),
            Role = value.GetProperty("Role").GetString() ?? "user",
            Content = value.GetProperty("Content").GetString() ?? string.Empty,
            CreatedAt = value.TryGetProperty("CreatedAt", out var createdAt) ? createdAt.GetDateTimeOffset() : DateTimeOffset.UtcNow
        };
    }

    public override void Write(Utf8JsonWriter writer, ChatMessageState value, JsonSerializerOptions options)
    {
        writer.WriteStartObject();
        writer.WriteString("Id", value.Id);
        writer.WriteString("Role", value.Role);
        writer.WriteString("Content", value.Content);
        writer.WriteString("CreatedAt", value.CreatedAt);
        writer.WriteEndObject();
    }
}
