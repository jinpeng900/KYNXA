using System.Text.Json;
using Windows.Storage;

namespace KYNXA_Desktop.Services;

public sealed record AppearanceTransferPreferences(string PaletteId, string Mode, double ChatFontSize, string? CustomAccent);

/// <summary>
/// Transfers only saved presentation preferences through a bounded, versioned document.
/// 使用有容量限制和版本标记的文件，仅传递已保存的界面外观偏好。
/// </summary>
public static class AppearancePreferencesTransfer
{
    public const int CurrentVersion = 1;
    public const int MaximumFileBytes = 16 * 1024;
    private const string DocumentFormat = "kynxa.appearance";

    public static byte[] Serialize(AppearanceTransferPreferences preferences)
    {
        Validate(preferences);
        return JsonSerializer.SerializeToUtf8Bytes(new
        {
            format = DocumentFormat,
            version = CurrentVersion,
            paletteId = preferences.PaletteId,
            mode = preferences.Mode,
            chatFontSize = preferences.ChatFontSize,
            customAccent = AppearanceService.NormalizeAccent(preferences.CustomAccent)
        }, new JsonSerializerOptions { WriteIndented = true });
    }

    public static AppearanceTransferPreferences Parse(ReadOnlyMemory<byte> bytes)
    {
        if (bytes.Length == 0 || bytes.Length > MaximumFileBytes)
            throw new InvalidDataException("Appearance preference file exceeds the size limit or is empty.");
        // UTF-8 BOM is accepted for JSON files saved by common Windows editors.
        // 接受常见 Windows 编辑器保存的 UTF-8 BOM，但不改写文件内容。
        if (bytes.Span.StartsWith(new byte[] { 0xEF, 0xBB, 0xBF })) bytes = bytes[3..];
        using var document = JsonDocument.Parse(bytes, new JsonDocumentOptions { MaxDepth = 4 });
        var root = document.RootElement;
        if (root.ValueKind != JsonValueKind.Object) throw new InvalidDataException("Invalid appearance document.");
        var expected = new HashSet<string>(["format", "version", "paletteId", "mode", "chatFontSize", "customAccent"], StringComparer.Ordinal);
        foreach (var property in root.EnumerateObject())
            if (!expected.Remove(property.Name)) throw new InvalidDataException("Unknown or duplicate appearance property.");
        if (expected.Count != 0 || ReadString(root, "format") != DocumentFormat)
            throw new InvalidDataException("Invalid appearance document format.");
        if (!root.GetProperty("version").TryGetInt32(out int version) || version != CurrentVersion)
            throw new InvalidDataException("Unsupported appearance document version.");
        if (!root.GetProperty("chatFontSize").TryGetDouble(out double fontSize))
            throw new InvalidDataException("Invalid appearance font size.");
        var accent = root.GetProperty("customAccent");
        string? customAccent = accent.ValueKind == JsonValueKind.Null ? null : ReadString(root, "customAccent");
        var preferences = new AppearanceTransferPreferences(ReadString(root, "paletteId"), ReadString(root, "mode"), fontSize, customAccent);
        Validate(preferences);
        return preferences with { CustomAccent = AppearanceService.NormalizeAccent(customAccent) };
    }

    private static string ReadString(JsonElement root, string name)
    {
        var value = root.GetProperty(name);
        return value.ValueKind == JsonValueKind.String && value.GetString() is { } text
            ? text : throw new InvalidDataException("Invalid appearance string property.");
    }

    private static void Validate(AppearanceTransferPreferences preferences)
    {
        // Imports reject unsupported values instead of silently normalizing them into different choices.
        // 导入时拒绝不支持的值，不静默回退成用户没有选择过的外观。
        if (!AppearanceService.Palettes.Any(palette => palette.Id == preferences.PaletteId)
            || preferences.Mode is not ("light" or "dark" or "system")
            || !double.IsFinite(preferences.ChatFontSize)
            || preferences.ChatFontSize < AppearanceService.MinimumChatFontSize
            || preferences.ChatFontSize > AppearanceService.MaximumChatFontSize
            || preferences.ChatFontSize * 2 != Math.Truncate(preferences.ChatFontSize * 2)
            || preferences.CustomAccent is not null && (preferences.CustomAccent.Length != 7
                || AppearanceService.NormalizeAccent(preferences.CustomAccent) is null))
            throw new InvalidDataException("Appearance preferences contain an unsupported value.");
    }

    public static async Task<AppearanceTransferPreferences> ReadAsync(StorageFile file)
    {
        if (!file.FileType.Equals(".json", StringComparison.OrdinalIgnoreCase)
            || (await file.GetBasicPropertiesAsync()).Size > MaximumFileBytes)
            throw new InvalidDataException("Invalid appearance preference file type or size.");
        using var stream = await file.OpenStreamForReadAsync();
        byte[] bytes = new byte[MaximumFileBytes + 1];
        int count = 0;
        while (count < bytes.Length)
        {
            int read = await stream.ReadAsync(bytes.AsMemory(count));
            if (read == 0) break;
            count += read;
        }
        return Parse(bytes.AsMemory(0, count));
    }

    public static async Task WriteAsync(StorageFile file, AppearanceTransferPreferences preferences)
    {
        if (!file.FileType.Equals(".json", StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("The appearance export must use the JSON extension.");
        await FileIO.WriteBytesAsync(file, Serialize(preferences));
    }
}
