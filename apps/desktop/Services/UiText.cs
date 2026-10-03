namespace KYNXA_Desktop.Services;

/// <summary>Translations for application chrome only; user and model content stays unchanged.</summary>
public static partial class UiText
{
    public static string Language { get; private set; } = "zh-CN";
    public static event EventHandler? LanguageChanged;

    public static string NormalizeLanguage(string? language) => language == "en" ? "en" : "zh-CN";

    public static void Initialize(string? language)
    {
        string normalized = NormalizeLanguage(language);
        if (normalized == Language) return;
        Language = normalized;
        LanguageChanged?.Invoke(null, EventArgs.Empty);
    }

    public static string Get(string chinese) => Get(chinese, Language);

    public static string Get(string chinese, string? language)
    {
        if (NormalizeLanguage(language) != "en") return chinese;
        return English.TryGetValue(chinese, out string? translated)
            || ModelTranslations.TryGetValue(chinese, out translated)
            || RuntimeTranslations.TryGetValue(chinese, out translated)
            || MemoryTranslations.TryGetValue(chinese, out translated)
                ? translated : chinese;
    }
}
