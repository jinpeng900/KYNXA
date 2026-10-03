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
