using System.Net;
using System.Reflection;
using System.Text.Json;
using System.Text.RegularExpressions;
using KYNXA_Desktop.Services;

int checks = 0;
void Check(bool condition, string message)
{
    if (!condition) throw new InvalidOperationException(message);
    checks++;
}

UiText.Initialize(null);
Check(UiText.Language == "zh-CN" && UiText.Get("设置") == "设置", "Old configurations must retain Chinese.");
foreach (string? invalid in new string?[] { null, "", "fr", "invalid" })
    Check(UiText.NormalizeLanguage(invalid) == "zh-CN", "Unknown language must fall back to Chinese.");
int changes = 0;
EventHandler changed = (_, _) => changes++;
UiText.LanguageChanged += changed;
UiText.Initialize("en");
Check(UiText.Get("设置") == "Settings", "English settings label is missing.");
Check(changes == 1, "A language change must notify open views.");
UiText.Initialize("en");
Check(changes == 1, "Selecting the same language must not refresh open views.");
Check(UiText.Get("设置", "zh-CN") == "设置" && UiText.Language == "en", "Preview must not change the running interface language.");
Check(changes == 1, "A lookup must not emit a language-change notification.");
Check(UiText.Get("unregistered label") == "unregistered label", "Unknown labels must remain readable.");
string userName = "研究 {1} / English \\ 中文";
Check(string.Format(UiText.Get("已归档“{0}”"), userName) == $"Archived “{userName}”", "User text must remain intact in translated templates.");

var translations = new Dictionary<string, string>(StringComparer.Ordinal);
foreach (var field in typeof(UiText).GetFields(BindingFlags.Static | BindingFlags.NonPublic))
{
    if (field.GetValue(null) is not IReadOnlyDictionary<string, string> dictionary) continue;
    foreach (var (key, value) in dictionary)
    {
        Check(!string.IsNullOrWhiteSpace(value), $"Empty translation: {key}");
        Check(!Regex.IsMatch(value, @"[\p{IsCJKUnifiedIdeographs}]"), $"Chinese text in English translation: {key}");
        if (translations.TryGetValue(key, out var existing)) Check(existing == value, $"Conflicting translations: {key}");
        translations[key] = value;
        var sourceArgs = Regex.Matches(key, @"\{\d+(?:[^{}]*)\}").Select(m => m.Value).Order().ToArray();
        var translatedArgs = Regex.Matches(value, @"\{\d+(?:[^{}]*)\}").Select(m => m.Value).Order().ToArray();
        Check(sourceArgs.SequenceEqual(translatedArgs), $"Format placeholders changed: {key}");
        if (sourceArgs.Length > 0) _ = string.Format(value, Enumerable.Repeat<object>("测试 {0}", 16).ToArray());
    }
}
Check(translations.Count > 100, "The interface translation catalog was not linked.");

DirectoryInfo? root = new(AppContext.BaseDirectory);
while (root is not null && !Directory.Exists(Path.Combine(root.FullName, "apps", "desktop"))) root = root.Parent;
Check(root is not null, "Run this smoke test from the repository.");
string desktop = Path.Combine(root!.FullName, "apps", "desktop");
var getPattern = new Regex("UiText\\.Get\\(\\s*(\"(?:\\\\.|[^\"\\\\])*\")");
var markupPattern = new Regex(@"\{services:UiText(?:Extension)? Key='([^']+)'\}");
var liveMarkupPattern = new Regex("services:UiLocalization\\.\\w+=\"([^\"]+)\"");
int references = 0;
foreach (string file in Directory.EnumerateFiles(desktop, "*", SearchOption.AllDirectories)
    .Where(p => (p.EndsWith(".cs") || p.EndsWith(".xaml")) && !p.Contains(Path.DirectorySeparatorChar + "obj" + Path.DirectorySeparatorChar)
        && !p.Contains(Path.DirectorySeparatorChar + "bin" + Path.DirectorySeparatorChar)))
{
    string source = File.ReadAllText(file);
    var keys = getPattern.Matches(source).Select(m => JsonSerializer.Deserialize<string>(m.Groups[1].Value)!)
        .Concat(markupPattern.Matches(source).Select(m => WebUtility.HtmlDecode(m.Groups[1].Value)))
        .Concat(liveMarkupPattern.Matches(source).Select(m => WebUtility.HtmlDecode(m.Groups[1].Value)));
    foreach (string key in keys)
    {
        Check(translations.ContainsKey(key), $"Missing English translation in {Path.GetFileName(file)}: {key}");
        references++;
    }
}
Check(references > 100, "Localization references were not found.");
UiText.Initialize("zh-CN");
Check(UiText.Get("设置") == "设置", "Switching back must restore Chinese.");
Check(changes == 2, "Switching back must notify the same open views.");
UiText.LanguageChanged -= changed;
Console.WriteLine($"UI language smoke passed: {checks} checks, {translations.Count} labels, {references} source references.");
