using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace KYNXA_Desktop.Services;

/// <summary>Only extension-owned paths change; external installations and unknown disabled IDs stay intact.</summary>
internal static class ExtensionConfigurationMigration
{
    internal static async Task ValidateAsync(string root, CancellationToken cancellationToken)
    {
        string path = Path.Combine(root, "Agent", "config.json");
        if (!File.Exists(path))
        {
            if (Directory.Exists(path)) throw InvalidConfig();
            return;
        }
        StorageMigrationService.RejectLinks(path);
        if (new FileInfo(path).Length > 256 * 1024) throw InvalidConfig();
        _ = ParseConfig(await ReadConfigAsync(path, cancellationToken));
    }

    internal static async Task RelocateAgentConfigAsync(string sourceRoot, string targetRoot, CancellationToken cancellationToken)
    {
        await RelocateUvRuntimeConfigAsync(sourceRoot, targetRoot, cancellationToken);
        string path = Path.Combine(targetRoot, "Agent", "config.json");
        if (!File.Exists(path)) return;
        StorageMigrationService.RejectLinks(path);
        var config = ParseConfig(await ReadConfigAsync(path, cancellationToken));
        string[] originalDirectories = config["skillDirectories"]!.AsArray().Select(node => node!.GetValue<string>()).ToArray();
        string? Relocate(string? value)
        {
            if (string.IsNullOrWhiteSpace(value) || !Path.IsPathFullyQualified(value)) return value;
            foreach (string name in new[] { "Agent", "Skills", "MCP" })
                if (StorageMigrationService.IsWithin(value, Path.Combine(sourceRoot, name)))
                    return Path.Combine(targetRoot, Path.GetRelativePath(sourceRoot, value));
            return value;
        }
        string? RelocateArgument(string? value)
        {
            if (value is null) return null;
            int equals = value.StartsWith("--", StringComparison.Ordinal) ? value.IndexOf('=') : -1;
            return equals < 0 ? Relocate(value) : value[..(equals + 1)] + Relocate(value[(equals + 1)..]);
        }
        void RelocateServerPaths(JsonObject obj)
        {
            foreach (string key in new[] { "command", "cwd" })
                if (obj[key] is JsonValue value && value.TryGetValue<string>(out string? original)) obj[key] = Relocate(original);
            if (obj["args"] is JsonArray args)
                for (int index = 0; index < args.Count; index++)
                    if (args[index] is JsonValue value && value.TryGetValue<string>(out string? original)) args[index] = RelocateArgument(original);
            if (obj["env"] is JsonObject environment)
                foreach (var pair in environment.ToArray())
                    if (pair.Value is JsonValue value && value.TryGetValue<string>(out string? original)) environment[pair.Key] = Relocate(original);
        }
        foreach (var server in config["mcpServers"]!.AsArray())
        {
            if (server is not JsonObject obj) throw InvalidConfig();
            RelocateServerPaths(obj);
        }
        if (config["officialMcpOverrides"] is JsonArray overrides)
            foreach (var item in overrides)
            {
                if (item is not JsonObject entry || (entry["changes"] ?? entry["server"]) is not JsonObject settings) throw InvalidConfig();
                RelocateServerPaths(settings);
            }
        var directories = config["skillDirectories"]!.AsArray();
        for (int index = 0; index < directories.Count; index++)
            directories[index] = Relocate(directories[index]!.GetValue<string>());

        var idMap = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (string name in new[] { "Agent", "Skills", "MCP" })
        {
            var owned = new StorageMigrationService.CopyRoot(Path.Combine(sourceRoot, name), name);
            foreach (string relative in StorageMigrationService.ListFiles(owned))
            {
                if (!string.Equals(Path.GetFileName(relative), "SKILL.md", StringComparison.OrdinalIgnoreCase)) continue;
                string original = Path.Combine(owned.Source, relative);
                idMap[SkillId(original)] = SkillId(Path.Combine(targetRoot, name, relative));
                // Discovery hashes lexical configured paths, including their original casing.
                foreach (string directory in originalDirectories)
                    if (StorageMigrationService.IsWithin(directory, owned.Source) && StorageMigrationService.IsWithin(original, directory))
                    {
                        string alias = Path.Combine(directory, Path.GetRelativePath(directory, original));
                        idMap[SkillId(alias)] = SkillId(Relocate(alias)!);
                    }
            }
        }
        if (config["disabledSkills"] is JsonArray disabled)
            for (int index = 0; index < disabled.Count; index++)
            {
                string id = disabled[index]!.GetValue<string>();
                if (idMap.TryGetValue(id, out string? relocated)) disabled[index] = relocated;
            }
        config["revision"] = config["revision"]!.GetValue<long>() + 1;
        await File.WriteAllTextAsync(path, config.ToJsonString(new JsonSerializerOptions { WriteIndented = true }), cancellationToken);
    }

    // uv-managed Windows environments keep their base interpreter in pyvenv.cfg.
    // Only these known text metadata fields change: wheel contents, launchers,
    // requirements, arbitrary package configuration and external paths remain exact.
    private static async Task RelocateUvRuntimeConfigAsync(string sourceRoot, string targetRoot, CancellationToken cancellationToken)
    {
        string sourceMcp = Path.Combine(sourceRoot, "MCP");
        foreach (string name in new[] { "uv-tools", "uv-cache" })
        {
            var owned = new StorageMigrationService.CopyRoot(Path.Combine(targetRoot, "MCP", name), name);
            foreach (string relative in StorageMigrationService.ListFiles(owned))
            {
                cancellationToken.ThrowIfCancellationRequested();
                string fileName = Path.GetFileName(relative);
                bool environment = string.Equals(fileName, "pyvenv.cfg", StringComparison.OrdinalIgnoreCase);
                bool receipt = string.Equals(fileName, "uv-receipt.toml", StringComparison.OrdinalIgnoreCase);
                if (!environment && !receipt) continue;
                string path = Path.Combine(owned.Source, relative);
                StorageMigrationService.RejectLinks(path);
                if (new FileInfo(path).Length > 256 * 1024) throw InvalidConfig();
                string original = await ReadConfigAsync(path, cancellationToken);
                string changed = environment ? RelocateEnvironment(original) : RelocateReceipt(original);
                if (changed == original) continue;
                await ReplaceTargetTextAsync(path, original, changed, cancellationToken);
            }
        }

        string? RelocateRuntimePath(string value, bool? directory = null)
        {
            if (string.IsNullOrWhiteSpace(value) || !Path.IsPathFullyQualified(value)
                || !StorageMigrationService.IsWithin(value, sourceMcp)) return null;
            string relocated = Path.Combine(targetRoot, "MCP", Path.GetRelativePath(sourceMcp, value));
            StorageMigrationService.RejectLinks(relocated);
            bool exists = directory switch
            {
                true => Directory.Exists(relocated),
                false => File.Exists(relocated),
                null => File.Exists(relocated) || Directory.Exists(relocated)
            };
            if (!exists) throw InvalidConfig();
            return relocated;
        }

        string RelocateEnvironment(string text)
        {
            return System.Text.RegularExpressions.Regex.Replace(text,
                @"(?m)^(?<prefix>[\uFEFF \t]*(?<key>home|executable)[ \t]*=[ \t]*)(?<value>[^\r\n]*?)(?<suffix>[ \t]*)(?=\r?$)", match =>
                {
                    string? relocated = RelocateRuntimePath(match.Groups["value"].Value,
                        directory: match.Groups["key"].Value == "home");
                    return relocated is null ? match.Value : match.Groups["prefix"].Value + relocated + match.Groups["suffix"].Value;
                });
        }

        string RelocateReceipt(string text)
        {
            // uv 0.12 writes [tool].python and entrypoints=[{install-path=...}].
            // Token boundaries prevent quoted requirements/comments from matching keys.
            var tokens = TokenizeToml(text);
            var replacements = new List<(int Start, int Length, string Text)>();
            bool toolTable = false, statementStart = true;
            for (int index = 0; index < tokens.Count; index++)
            {
                var token = tokens[index];
                if (token.Value == "\n") { statementStart = true; continue; }
                if (statementStart && token.Value == "[")
                {
                    int end = index;
                    while (end < tokens.Count && tokens[end].Value != "\n") end++;
                    toolTable = end - index == 3 && tokens[index + 1].Value == "tool" && tokens[index + 2].Value == "]";
                    index = end - 1;
                    statementStart = false;
                    continue;
                }
                if (toolTable && statementStart && token.Value == "python" && index + 2 < tokens.Count
                    && tokens[index + 1].Value == "=" && (index + 3 == tokens.Count || tokens[index + 3].Value == "\n"))
                    ReplaceString(tokens[index + 2]);
                if (toolTable && statementStart && token.Value == "entrypoints" && index + 2 < tokens.Count
                    && tokens[index + 1].Value == "=" && tokens[index + 2].Value == "[")
                {
                    int arrayDepth = 0, objectDepth = 0;
                    for (index += 2; index < tokens.Count; index++)
                    {
                        var entry = tokens[index];
                        if (entry.Value == "[") arrayDepth++;
                        if (entry.Value == "]" && --arrayDepth == 0) break;
                        if (entry.Value == "{") objectDepth++;
                        if (entry.Value == "}") objectDepth--;
                        if (entry.Kind == TomlTokenKind.Word && entry.Value == "install-path" && arrayDepth == 1 && objectDepth == 1
                            && index + 2 < tokens.Count && tokens[index + 1].Value == "=")
                            ReplaceString(tokens[index + 2]);
                    }
                }
                statementStart = false;
            }
            var result = new StringBuilder(text);
            foreach (var replacement in replacements.OrderByDescending(value => value.Start))
                result.Remove(replacement.Start, replacement.Length).Insert(replacement.Start, replacement.Text);
            return result.ToString();

            void ReplaceString(TomlToken token)
            {
                if (token.Kind != TomlTokenKind.String) return;
                string literal = token.Value;
                string value;
                try { value = literal[0] == '\'' ? literal[1..^1] : JsonSerializer.Deserialize<string>(literal)!; }
                catch (JsonException error) { throw new InvalidDataException(UiText.Get("工具配置文件损坏，原文件已保留。"), error); }
                string? relocated = RelocateRuntimePath(value);
                if (relocated is null) return;
                // JSON basic strings are valid TOML basic strings; never substitute inside a binary or arbitrary string.
                replacements.Add((token.Start, token.Value.Length, JsonSerializer.Serialize(relocated.Replace('\\', '/'))));
            }
        }
    }

    private enum TomlTokenKind { Word, Symbol, String, OpaqueString }
    private sealed record TomlToken(TomlTokenKind Kind, int Start, string Value);

    private static List<TomlToken> TokenizeToml(string text)
    {
        var tokens = new List<TomlToken>();
        for (int index = 0; index < text.Length;)
        {
            char current = text[index];
            if (current is ' ' or '\t' or '\r' or '\uFEFF') { index++; continue; }
            if (current == '#') { while (index < text.Length && text[index] != '\n') index++; continue; }
            int start = index++;
            if (current is '\'' or '"')
            {
                bool multiline = index + 1 < text.Length && text[index] == current && text[index + 1] == current;
                if (multiline) index += 2;
                bool closed = false;
                while (index < text.Length)
                {
                    if (current == '"' && text[index] == '\\') { index += Math.Min(2, text.Length - index); continue; }
                    if (text[index] == current && (!multiline || index + 2 < text.Length && text[index + 1] == current && text[index + 2] == current))
                    {
                        index += multiline ? 3 : 1;
                        closed = true;
                        break;
                    }
                    index++;
                }
                if (!closed) throw InvalidConfig();
                tokens.Add(new(multiline ? TomlTokenKind.OpaqueString : TomlTokenKind.String, start, text[start..index]));
            }
            else if (char.IsLetterOrDigit(current) || current is '_' or '-')
            {
                while (index < text.Length && (char.IsLetterOrDigit(text[index]) || text[index] is '_' or '-' or '.')) index++;
                tokens.Add(new(TomlTokenKind.Word, start, text[start..index]));
            }
            else tokens.Add(new(TomlTokenKind.Symbol, start, current.ToString()));
        }
        return tokens;
    }

    private static async Task ReplaceTargetTextAsync(string path, string original, string changed, CancellationToken cancellationToken)
    {
        string temporary = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            await using (var output = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None))
            {
                byte[] bytes = new UTF8Encoding(false).GetBytes(changed);
                await output.WriteAsync(bytes, cancellationToken);
                await output.FlushAsync(cancellationToken);
            }
            StorageMigrationService.RejectLinks(path);
            if (await ReadConfigAsync(path, cancellationToken) != original)
                throw new IOException(UiText.Get("扩展迁移期间配置发生变化，请停止其他 KYNXA 实例后重试。"));
            cancellationToken.ThrowIfCancellationRequested();
            File.Move(temporary, path, overwrite: true);
        }
        finally { if (File.Exists(temporary)) File.Delete(temporary); }
    }

    internal static string SkillId(string path) => Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(Path.GetFullPath(path))))[..24];

    private static async Task<string> ReadConfigAsync(string path, CancellationToken cancellationToken)
    {
        try { return new UTF8Encoding(false, true).GetString(await File.ReadAllBytesAsync(path, cancellationToken)); }
        catch (DecoderFallbackException error) { throw new InvalidDataException(UiText.Get("工具配置文件损坏，原文件已保留。"), error); }
    }

    private static JsonObject ParseConfig(string text)
    {
        try
        {
            var config = JsonNode.Parse(text.TrimStart('\uFEFF')) as JsonObject ?? throw InvalidConfig();
            if (config["version"] is not JsonValue version || !version.TryGetValue<int>(out int number) || number != 1)
                throw new InvalidDataException(UiText.Get("工具配置版本不受支持，原文件已保留。"));
            if (config["revision"] is not JsonValue revision || !revision.TryGetValue<long>(out long counter) || counter < 0 || counter >= 9007199254740991L
                || config["mcpServers"] is not JsonArray servers || servers.Count > 32
                || config["skillDirectories"] is not JsonArray directories || directories.Count > 32
                || directories.Any(node => node is not JsonValue value || !value.TryGetValue<string>(out string? path) || string.IsNullOrWhiteSpace(path) || !Path.IsPathFullyQualified(path)))
                throw InvalidConfig();
            if (config["disabledSkills"] is JsonNode disabled && (disabled is not JsonArray list || list.Count > 4096 || list.Any(node =>
                node is not JsonValue value || !value.TryGetValue<string>(out string? id) || id.Length != 24 || id.Any(c => !"0123456789abcdef".Contains(c)))))
                throw InvalidConfig();
            if (config["disabledOfficialMcpServers"] is JsonNode hidden &&
                (hidden is not JsonArray hiddenList || hiddenList.Count > 64 || hiddenList.Any(node => !IsPresetId(node))))
                throw InvalidConfig();
            if (config["officialMcpOverrides"] is JsonNode overrides &&
                (overrides is not JsonArray overrideList || overrideList.Count > 64 || overrideList.Any(node =>
                    node is not JsonObject entry || !IsPresetId(entry["presetId"]) ||
                    (entry["changes"] ?? entry["server"]) is not JsonObject))) throw InvalidConfig();
            return config;
        }
        catch (JsonException error) { throw new InvalidDataException(UiText.Get("工具配置文件损坏，原文件已保留。"), error); }
    }

    private static bool IsPresetId(JsonNode? node) => node is JsonValue value && value.TryGetValue<string>(out string? id) &&
        id.Length is >= 2 and <= 40 && id[0] is >= 'a' and <= 'z' && id.All(character => character is >= 'a' and <= 'z' or >= '0' and <= '9' or '-');

    private static InvalidDataException InvalidConfig() => new(UiText.Get("工具配置文件损坏，原文件已保留。"));
}
