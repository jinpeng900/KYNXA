using System.ComponentModel;
using System.Net;
using System.Net.Sockets;

namespace KYNXA_Desktop.Services;

public sealed record ModelPreset(string Id, string Name, string BaseUrl, string[] Models, string Hint,
    string Protocol = "openai-completions") : INotifyPropertyChanged
{
    public string DisplayName => UiText.Get(Name);
    public event PropertyChangedEventHandler? PropertyChanged;

    internal void RefreshDisplayName() => PropertyChanged?.Invoke(this, new(nameof(DisplayName)));

    public override string ToString() => DisplayName;
}

public static class ModelPresets
{
    // Official API references are recorded in UI-COMPONENTS.md. Never store keys here.
    public static IReadOnlyList<ModelPreset> All { get; } = new ModelPreset[]
    {
        new("deepseek", "DeepSeek", "https://api.deepseek.com",
            ModelCatalog.Ids("deepseek"), "地址和模型已填好，输入 API Key 即可保存。"),
        new("kimi", "Kimi", "https://api.moonshot.cn/v1",
            ModelCatalog.Ids("kimi"), "适用于 Kimi 国内开放平台的 API Key。"),
        new("openai", "OpenAI", "https://api.openai.com/v1",
            ModelCatalog.Ids("openai"),
            "使用 OpenAI API Key，通过 Responses 接口调用。", "openai-responses"),
        new("anthropic", "Anthropic / Claude", "https://api.anthropic.com/v1",
            ModelCatalog.Ids("anthropic"),
            "使用 Anthropic API Key，直接连接 Claude Messages 接口。", "anthropic-messages"),
        new("gemini", "Google / Gemini", "https://generativelanguage.googleapis.com/v1beta/openai",
            ModelCatalog.Ids("gemini"), "使用 Google AI Studio 的 Gemini API Key。可获取账号支持的其他模型。"),
        new("qwen", "阿里云百炼 / Qwen", "https://dashscope.aliyuncs.com/compatible-mode/v1",
            ModelCatalog.Ids("qwen"),
            "默认国内北京区域；也可填写控制台提供的业务空间地址，须与密钥区域一致。"),
        new("zhipu", "智谱 / GLM", "https://open.bigmodel.cn/api/paas/v4",
            ModelCatalog.Ids("zhipu"),
            "智谱开放平台的标准 API，使用对应平台的 API Key。"),
        new("minimax", "MiniMax", "https://api.minimax.cn/v1",
            ModelCatalog.Ids("minimax"), "默认国内开放平台地址；模型访问范围以账号为准。"),
        new("xai", "xAI / Grok", "https://api.x.ai/v1",
            ModelCatalog.Ids("xai"), "使用 xAI API Key，支持获取账号可用模型。", "openai-responses"),
        new("local-api", "本地 API（直接连接）", "http://127.0.0.1:8080/v1", [],
            "填写本机或局域网模型服务地址，直接调用兼容 OpenAI 的接口，无需 Ollama。"),
        new("ollama", "Ollama", "http://127.0.0.1:11434/v1", [],
            "启动本机 Ollama 后，点击「获取模型」读取已安装的模型，无需密钥。"),
        new("lmstudio", "LM Studio", "http://127.0.0.1:1234/v1", [],
            "开启 LM Studio 的本地服务后，点击「获取模型」。"),
        new("custom", "自定义服务", "", [],
            "填写服务地址和模型 ID，接入其他兼容 OpenAI 的服务。")
    };

    // Notify existing items so the open selector keeps its selection and container state.
    public static void RefreshDisplayNames()
    {
        foreach (var preset in All) preset.RefreshDisplayName();
    }

    // Context-only UI defaults mirror the verified gateway capability snapshot.
    // Enforcement and independent input/output ceilings belong to model-capabilities.mjs.
    private static readonly IReadOnlyDictionary<string, Dictionary<string, int>> OfficialContextWindows = BuildContextWindows();

    private static IReadOnlyDictionary<string, Dictionary<string, int>> BuildContextWindows()
    {
        var hosts = new Dictionary<string, Dictionary<string, int>>(StringComparer.Ordinal);
        void Add(string host, int window, params string[] models)
        {
            if (!hosts.TryGetValue(host, out var entries)) hosts[host] = entries = new(StringComparer.Ordinal);
            foreach (string model in models) entries[model] = window;
        }
        Add("api.deepseek.com", 1_048_576, "deepseek-flash", "deepseek-v4-pro");
        Add("api.moonshot.cn", 1_048_576, "kimi-k3");
        Add("api.moonshot.cn", 262_144, "kimi-k2.7-code", "kimi-k2.7-code-highspeed", "kimi-k2.6");
        Add("api.anthropic.com", 1_000_000, "claude-opus-5-5", "claude-sonnet-5-5", "claude-fable-5-1",
            "claude-opus-5", "claude-sonnet-5", "claude-fable-5", "claude-opus-4-8", "claude-opus-4-7",
            "claude-opus-4-6", "claude-sonnet-4-6");
        Add("api.anthropic.com", 200_000, "claude-haiku-4-5", "claude-haiku-4-5-20251001",
            "claude-opus-4-5", "claude-opus-4-5-20251101", "claude-sonnet-4-5", "claude-sonnet-4-5-20250929");
        Add("api.openai.com", 1_050_000, "gpt-6-astra", "gpt-6.1-sol", "gpt-6-sol", "gpt-6-luna",
            "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.5", "gpt-5.5-2026-04-23",
            "gpt-5.5-pro", "gpt-5.5-pro-2026-04-23", "gpt-5.4", "gpt-5.4-2026-03-05",
            "gpt-5.4-pro", "gpt-5.4-pro-2026-03-05");
        Add("api.openai.com", 400_000, "gpt-5.4-mini", "gpt-5.4-mini-2026-03-17", "gpt-5.4-nano",
            "gpt-5.4-nano-2026-03-17", "gpt-5.3-codex", "gpt-5.2", "gpt-5.2-2025-12-11",
            "gpt-5.2-pro", "gpt-5.2-pro-2025-12-11", "gpt-5.1", "gpt-5.1-2025-11-13", "gpt-5",
            "gpt-5-2025-08-07", "gpt-5-mini", "gpt-5-mini-2025-08-07", "gpt-5-nano",
            "gpt-5-nano-2025-08-07", "gpt-5-pro", "gpt-5-pro-2025-10-06", "chat-latest");
        Add("api.openai.com", 200_000, "o3", "o3-2025-04-16", "o3-pro", "o3-pro-2025-06-10");
        Add("api.openai.com", 1_047_576, "gpt-4.1", "gpt-4.1-2025-04-14", "gpt-4.1-mini", "gpt-4.1-mini-2025-04-14");
        Add("api.openai.com", 128_000, "gpt-4o", "gpt-4o-2024-08-06", "gpt-4o-mini", "gpt-4o-mini-2024-07-18");
        return hosts;
    }

    /// <summary>A new draft uses the smallest verified window among its selected models.</summary>
    public static int DefaultContextWindowTokens(string baseUrl, IEnumerable<string> selectedModels)
    {
        if (!Uri.TryCreate(baseUrl, UriKind.Absolute, out var uri) || uri.Scheme != Uri.UriSchemeHttps ||
            !uri.IsDefaultPort || uri.UserInfo.Length != 0 || uri.Query.Length != 0 || uri.Fragment.Length != 0 ||
            uri.AbsolutePath is not ("/" or "/v1" or "/v1/") || !OfficialContextWindows.TryGetValue(uri.Host, out var limits))
            return ModelApiClient.DefaultContextWindowTokens;
        int smallest = int.MaxValue;
        foreach (string model in selectedModels)
        {
            if (!limits.TryGetValue(model, out int window)) return ModelApiClient.DefaultContextWindowTokens;
            smallest = Math.Min(smallest, window);
        }
        return smallest == int.MaxValue ? ModelApiClient.DefaultContextWindowTokens : smallest;
    }

    public static bool IsLocalEndpoint(Uri uri)
    {
        if (uri.Host.Equals("localhost", StringComparison.OrdinalIgnoreCase)) return true;
        if (!IPAddress.TryParse(uri.Host.Trim('[', ']'), out var address)) return false;
        if (address.IsIPv4MappedToIPv6) address = address.MapToIPv4();
        if (IPAddress.IsLoopback(address)) return true;
        var bytes = address.GetAddressBytes();
        return address.AddressFamily == AddressFamily.InterNetwork
            ? bytes[0] == 10 || (bytes[0] == 172 && bytes[1] is >= 16 and <= 31) ||
              (bytes[0] == 192 && bytes[1] == 168)
            : (bytes[0] & 0xfe) == 0xfc;
    }

    public static ModelPreset? Recognize(string name) => All.FirstOrDefault(p => p.Id != "custom" &&
        (string.Equals(name.Trim(), p.Id, StringComparison.OrdinalIgnoreCase) ||
         string.Equals(name.Trim(), p.Name, StringComparison.OrdinalIgnoreCase) ||
         string.Equals(name.Trim(), $"{p.Name} API", StringComparison.OrdinalIgnoreCase) ||
         string.Equals(name.Trim(), UiText.Get(p.Name, "en"), StringComparison.OrdinalIgnoreCase) ||
         string.Equals(name.Trim(), $"{UiText.Get(p.Name, "en")} API", StringComparison.OrdinalIgnoreCase)));

    public static string UniqueId(string prefix, IEnumerable<string> existing)
    {
        var used = existing.ToHashSet(StringComparer.OrdinalIgnoreCase);
        string candidate = prefix;
        for (int suffix = 2; used.Contains(candidate); suffix++) candidate = $"{prefix}-{suffix}";
        return candidate;
    }
}
