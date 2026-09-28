using System.Net;
using System.Net.Sockets;

namespace KYNXA_Desktop.Services;

public sealed record ModelPreset(string Id, string Name, string BaseUrl, string[] Models, string Hint,
    string Protocol = "openai-completions")
{
    public override string ToString() => Name;
}

public static class ModelPresets
{
    // Official API references are recorded in UI-COMPONENTS.md. Never store keys here.
    public static IReadOnlyList<ModelPreset> All { get; } = new ModelPreset[]
    {
        new("deepseek", "DeepSeek", "https://api.deepseek.com",
            ["deepseek-flash", "deepseek-v4-pro"], "地址和模型已填好，输入 API Key 即可保存。"),
        new("kimi", "Kimi", "https://api.moonshot.cn/v1",
            ["kimi-k3", "kimi-k2.7-code", "kimi-k2.6"], "适用于 Kimi 国内开放平台的 API Key。"),
        new("openai", "OpenAI", "https://api.openai.com/v1",
            ["gpt-6-astra", "gpt-6-sol", "gpt-6-luna", "gpt-5.4"],
            "使用 OpenAI API Key，通过 Responses 接口调用。", "openai-responses"),
        new("anthropic", "Anthropic / Claude", "https://api.anthropic.com/v1",
            ["claude-opus-5-5", "claude-sonnet-5", "claude-fable-5-1", "claude-haiku-4-5-20251001"],
            "使用 Anthropic API Key，直接连接 Claude Messages 接口。", "anthropic-messages"),
        new("gemini", "Google / Gemini", "https://generativelanguage.googleapis.com/v1beta/openai",
            ["gemini-3.8-flash"], "使用 Google AI Studio 的 Gemini API Key。可获取账号支持的其他模型。"),
        new("qwen", "阿里云百炼 / Qwen", "https://dashscope.aliyuncs.com/compatible-mode/v1",
            ["qwen3.8-max", "qwen3.7-plus", "qwen3.7-flash"],
            "默认国内北京区域；也可填写控制台提供的业务空间地址，须与密钥区域一致。"),
        new("zhipu", "智谱 / GLM", "https://open.bigmodel.cn/api/paas/v4",
            ["glm-5.3", "glm-5.3-flash", "glm-5.1", "glm-5-turbo", "glm-4.7"],
            "智谱开放平台的标准 API，使用对应平台的 API Key。"),
        new("minimax", "MiniMax", "https://api.minimax.cn/v1",
            ["MiniMax-M3.1-Flash-Preview"], "默认国内开放平台地址；模型访问范围以账号为准。"),
        new("xai", "xAI / Grok", "https://api.x.ai/v1",
            ["grok-4.7"], "使用 xAI API Key，支持获取账号可用模型。", "openai-responses"),
        new("local-api", "本地 API（直接连接）", "http://127.0.0.1:8080/v1", [],
            "填写本机或局域网模型服务地址，直接调用兼容 OpenAI 的接口，无需 Ollama。"),
        new("ollama", "Ollama", "http://127.0.0.1:11434/v1", [],
            "启动本机 Ollama 后，点击「获取模型」读取已安装的模型，无需密钥。"),
        new("lmstudio", "LM Studio", "http://127.0.0.1:1234/v1", [],
            "开启 LM Studio 的本地服务后，点击「获取模型」。"),
        new("custom", "自定义服务", "", [],
            "填写服务地址和模型 ID，接入其他兼容 OpenAI 的服务。")
    };

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
         string.Equals(name.Trim(), $"{p.Name} API", StringComparison.OrdinalIgnoreCase)));

    public static string UniqueId(string prefix, IEnumerable<string> existing)
    {
        var used = existing.ToHashSet(StringComparer.OrdinalIgnoreCase);
        string candidate = prefix;
        for (int suffix = 2; used.Contains(candidate); suffix++) candidate = $"{prefix}-{suffix}";
        return candidate;
    }
}

public sealed record ModelDetail(string Id, string Name, string Description);

public static class ModelCatalog
{
    private static readonly ModelDetail[] Details =
    [
        new("deepseek-flash", "DeepSeek Flash", "日常问答、写作与代码任务"),
        new("deepseek-v4-pro", "DeepSeek V4 Pro", "复杂问题与深度推理"),
        new("kimi-k3", "Kimi K3", "长文档、知识工作与编程"),
        new("kimi-k2.7-code", "Kimi K2.7 Code", "代码生成与工程任务"),
        new("kimi-k2.6", "Kimi K2.6", "通用对话与任务处理"),
        new("gpt-6-astra", "GPT-6 Astra", "复杂推理与编程任务"),
        new("gpt-6-sol", "GPT-6 Sol", "兼顾能力与成本的通用选择"),
        new("gpt-6-luna", "GPT-6 Luna", "轻量、高频任务"),
        new("gpt-5.4", "GPT-5.4", "通用知识工作与代码任务"),
        new("claude-opus-5-5", "Claude Opus 5.5", "复杂编程与知识工作"),
        new("claude-sonnet-5", "Claude Sonnet 5", "兼顾响应速度与能力"),
        new("claude-fable-5-1", "Claude Fable 5.1", "高难度推理与长程任务"),
        new("claude-haiku-4-5-20251001", "Claude Haiku 4.5", "快速问答与轻量任务"),
        new("gemini-3.8-flash", "Gemini 3.8 Flash", "通用问答与内容处理"),
        new("qwen3.8-max", "Qwen 3.8 Max", "千问旗舰系列 · 云端"),
        new("qwen3.7-plus", "Qwen 3.7 Plus", "千问均衡系列 · 云端"),
        new("qwen3.7-flash", "Qwen 3.7 Flash", "千问快速系列 · 云端"),
        new("glm-5.3", "GLM-5.3", "编程与复杂任务"),
        new("glm-5.3-flash", "GLM-5.3 Flash", "通用内容处理"),
        new("glm-5.1", "GLM-5.1", "代码与长程任务"),
        new("glm-5-turbo", "GLM-5 Turbo", "长程任务与响应效率"),
        new("glm-4.7", "GLM-4.7", "通用对话与推理"),
        new("MiniMax-M3.1-Flash-Preview", "MiniMax M3.1 Flash Preview", "推理模型 · 预览版"),
        new("grok-4.7", "Grok 4.7", "通用问答与编程"),
        new("qwen3-8b", "Qwen3 8B（本地）", "8B 参数 · 本机推理服务"),
        new("qwen3:8b", "Qwen3 8B", "8B 参数 · 服务端模型 ID")
    ];

    public static ModelDetail Describe(string id) => Details.FirstOrDefault(model => model.Id == id)
        ?? new(id, id, "服务返回或手动添加的模型；能力以服务说明为准");
}
