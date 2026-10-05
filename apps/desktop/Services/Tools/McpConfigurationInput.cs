using System.Text.Json;
using System.Text.RegularExpressions;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Pure parsing of MCP editor references. Credentials are names, never values or model prompts.
/// 纯解析 MCP 编辑器引用；凭据只以名称保存，不包含实际值，也不进入模型提示。
/// </summary>
public static class McpConfigurationInput
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);

    public static Dictionary<string, string> ReadReferences(string text, bool headers)
    {
        var references = JsonSerializer.Deserialize<Dictionary<string, string>>(text) ?? throw new JsonException();
        foreach (var pair in references)
            if (!Regex.IsMatch(pair.Key, headers ? "^[!#$%&'*+.^_`|~0-9A-Za-z-]+$" : "^[A-Za-z_][A-Za-z0-9_]*$") ||
                !IsEnvironmentName(pair.Value)) throw new JsonException();
        return references;
    }

    public static McpAuthentication? ReadAuthentication(string text)
    {
        if (string.IsNullOrWhiteSpace(text)) return null;
        var auth = JsonSerializer.Deserialize<McpAuthentication>(text, JsonOptions) ?? throw new JsonException();
        if (auth.Type == "bearer-env" && IsEnvironmentName(auth.TokenEnv)) return auth;
        if (auth.Type == "oauth-client-credentials" && !string.IsNullOrWhiteSpace(auth.ClientId) && IsEnvironmentName(auth.ClientSecretEnv) &&
            Uri.TryCreate(auth.Issuer, UriKind.Absolute, out var issuer) && issuer.Scheme == "https" && issuer.UserInfo.Length == 0 &&
            issuer.Query.Length == 0 && issuer.Fragment.Length == 0) return auth;
        throw new JsonException();
    }

    private static bool IsEnvironmentName(string? value) => value is not null && Regex.IsMatch(value, "^[A-Za-z_][A-Za-z0-9_]*$");
}
