namespace KYNXA_Desktop.Services;

public sealed record ModelDetail(string Id, string Name);

/// <summary>
/// Official chat model IDs and names, checked 2026-10-01. Account access is discovered separately.
/// 官方聊天模型 ID 与名称核对于 2026-10-01；账户实际访问权限另行发现。
/// </summary>
public static class ModelCatalog
{
    // Keep each ID/name pair in one place; presets and picker labels share this catalog.
    // Excludes retired models, duplicate aliases and models requiring unrelated endpoints.
    // ID 与名称只维护一处，预设与选择器共用；排除退役模型、重复别名与需要其他端点的模型。
    private static readonly IReadOnlyDictionary<string, ModelDetail[]> ByProvider = new Dictionary<string, ModelDetail[]>
    {
        // https://api-docs.deepseek.com/quick_start/pricing/
        ["deepseek"] =
        [
            new("deepseek-flash", "DeepSeek V4.1 Flash"),
            new("deepseek-v4-pro", "DeepSeek V4 Pro"),
        ],
        // https://platform.kimi.com/docs/models
        ["kimi"] =
        [
            new("kimi-k3", "Kimi K3"),
            new("kimi-k2.7-code", "Kimi K2.7 Code"),
            new("kimi-k2.7-code-highspeed", "Kimi K2.7 Code Highspeed"),
            new("kimi-k2.6", "Kimi K2.6"),
        ],
        // https://developers.openai.com/api/docs/models/all
        ["openai"] =
        [
            new("gpt-6-astra", "GPT-6 Astra"),
            new("gpt-6.1-sol", "GPT-6.1 Sol"),
            new("gpt-6-sol", "GPT-6 Sol"),
            new("gpt-6-luna", "GPT-6 Luna"),
            new("gpt-5.6-sol", "GPT-5.6 Sol"),
            new("gpt-5.6-terra", "GPT-5.6 Terra"),
            new("gpt-5.6-luna", "GPT-5.6 Luna"),
            new("gpt-5.5", "GPT-5.5"),
            new("gpt-5.5-pro", "GPT-5.5 Pro"),
            new("gpt-5.4", "GPT-5.4"),
            new("gpt-5.4-pro", "GPT-5.4 Pro"),
            new("gpt-5.4-mini", "GPT-5.4 Mini"),
            new("gpt-5.4-nano", "GPT-5.4 Nano"),
            new("gpt-5.3-codex", "GPT-5.3 Codex"),
            new("gpt-5.2", "GPT-5.2"),
            new("gpt-5.2-pro", "GPT-5.2 Pro"),
            new("gpt-5.1", "GPT-5.1"),
            new("gpt-5", "GPT-5"),
            new("gpt-5-mini", "GPT-5 Mini"),
            new("gpt-5-nano", "GPT-5 Nano"),
            new("gpt-5-pro", "GPT-5 Pro"),
            new("o3", "o3"),
            new("o3-pro", "o3-pro"),
            new("gpt-4.1", "GPT-4.1"),
            new("gpt-4.1-mini", "GPT-4.1 Mini"),
            new("gpt-4o", "GPT-4o"),
            new("gpt-4o-mini", "GPT-4o Mini"),
            new("chat-latest", "Chat Latest"),
        ],
        // https://platform.claude.com/docs/en/about-claude/model-deprecations
        ["anthropic"] =
        [
            new("claude-opus-5-5", "Claude Opus 5.5"),
            new("claude-sonnet-5-5", "Claude Sonnet 5.5"),
            new("claude-fable-5-1", "Claude Fable 5.1"),
            new("claude-haiku-4-5-20251001", "Claude Haiku 4.5"),
            new("claude-opus-5", "Claude Opus 5"),
            new("claude-sonnet-5", "Claude Sonnet 5"),
            new("claude-fable-5", "Claude Fable 5"),
            new("claude-opus-4-8", "Claude Opus 4.8"),
            new("claude-opus-4-7", "Claude Opus 4.7"),
            new("claude-opus-4-6", "Claude Opus 4.6"),
            new("claude-sonnet-4-6", "Claude Sonnet 4.6"),
            new("claude-opus-4-5-20251101", "Claude Opus 4.5"),
            new("claude-sonnet-4-5-20250929", "Claude Sonnet 4.5"),
        ],
        // https://ai.google.dev/gemini-api/docs/models
        ["gemini"] =
        [
            new("gemini-3.8-flash", "Gemini 3.8 Flash"),
            new("gemini-3.7-flash", "Gemini 3.7 Flash"),
            new("gemini-3.6-flash", "Gemini 3.6 Flash"),
            new("gemini-3.5-flash", "Gemini 3.5 Flash"),
            new("gemini-3.5-flash-lite", "Gemini 3.5 Flash Lite"),
            new("gemini-3.1-flash-lite", "Gemini 3.1 Flash Lite"),
            new("gemini-3.1-pro-preview", "Gemini 3.1 Pro Preview"),
            new("gemini-3-flash-preview", "Gemini 3 Flash Preview"),
        ],
        // https://help.aliyun.com/zh/model-studio/rate-limit
        ["qwen"] =
        [
            new("qwen3.8-max", "Qwen3.8 Max"),
            new("qwen3.8-flash", "Qwen3.8 Flash"),
            new("qwen3.7-plus", "Qwen3.7 Plus"),
            new("qwen3.7-flash", "Qwen3.7 Flash"),
            new("qwen3.7-max", "Qwen3.7 Max"),
            new("qwen3.6-plus", "Qwen3.6 Plus"),
            new("qwen3.6-flash", "Qwen3.6 Flash"),
            new("qwen3.5-plus", "Qwen3.5 Plus"),
            new("qwen3.5-flash", "Qwen3.5 Flash"),
            new("qwen3-max", "Qwen3 Max"),
            new("qwen3-coder-plus", "Qwen3 Coder Plus"),
            new("qwen3-coder-flash", "Qwen3 Coder Flash"),
            new("qwen3-coder-next", "Qwen3 Coder Next"),
            new("qwen3-coder-480b-a35b-instruct", "Qwen3 Coder 480B A35B Instruct"),
            new("qwen3-coder-30b-a3b-instruct", "Qwen3 Coder 30B A3B Instruct"),
            new("qwen3.8-2.4t-a95b", "Qwen3.8 2.4T A95B"),
            new("qwen3.8-27b", "Qwen3.8 27B"),
            new("qwen3.6-35b-a3b", "Qwen3.6 35B A3B"),
            new("qwen3.6-27b", "Qwen3.6 27B"),
            new("qwen3.5-397b-a17b", "Qwen3.5 397B A17B"),
            new("qwen3.5-122b-a10b", "Qwen3.5 122B A10B"),
            new("qwen3.5-27b", "Qwen3.5 27B"),
            new("qwen3.5-35b-a3b", "Qwen3.5 35B A3B"),
            new("qwen3-next-80b-a3b-thinking", "Qwen3 Next 80B A3B Thinking"),
            new("qwen3-next-80b-a3b-instruct", "Qwen3 Next 80B A3B Instruct"),
            new("qwen3-235b-a22b-thinking-2507", "Qwen3 235B A22B Thinking 2507"),
            new("qwen3-235b-a22b-instruct-2507", "Qwen3 235B A22B Instruct 2507"),
            new("qwen3-30b-a3b-thinking-2507", "Qwen3 30B A3B Thinking 2507"),
            new("qwen3-30b-a3b-instruct-2507", "Qwen3 30B A3B Instruct 2507"),
            new("qwen3-235b-a22b", "Qwen3 235B A22B"),
            new("qwen3-30b-a3b", "Qwen3 30B A3B"),
            new("qwen3-32b", "Qwen3 32B"),
            new("qwen3-14b", "Qwen3 14B"),
            new("qwen3-8b", "Qwen3 8B"),
            new("qwen3-vl-plus", "Qwen3 VL Plus"),
            new("qwen3-vl-flash", "Qwen3 VL Flash"),
            new("qwen3-vl-235b-a22b-thinking", "Qwen3 VL 235B A22B Thinking"),
            new("qwen3-vl-235b-a22b-instruct", "Qwen3 VL 235B A22B Instruct"),
            new("qwen3-vl-32b-thinking", "Qwen3 VL 32B Thinking"),
            new("qwen3-vl-32b-instruct", "Qwen3 VL 32B Instruct"),
            new("qwen3-vl-30b-a3b-thinking", "Qwen3 VL 30B A3B Thinking"),
            new("qwen3-vl-30b-a3b-instruct", "Qwen3 VL 30B A3B Instruct"),
            new("qwen3-vl-8b-thinking", "Qwen3 VL 8B Thinking"),
            new("qwen3-vl-8b-instruct", "Qwen3 VL 8B Instruct"),
            new("qwen-plus", "Qwen Plus"),
            new("qwen-max", "Qwen Max"),
            new("qwen-flash", "Qwen Flash"),
            new("qwen-turbo", "Qwen Turbo"),
            new("qwq-plus", "QwQ Plus"),
            new("qwen-long", "Qwen Long"),
            new("qvq-max", "QVQ Max"),
            new("qvq-plus", "QVQ Plus"),
            new("qwen-vl-max", "Qwen VL Max"),
            new("qwen-vl-plus", "Qwen VL Plus"),
            new("qwen3.8-omni-flash", "Qwen3.8 Omni Flash"),
        ],
        // https://docs.bigmodel.cn/cn/guide/start/model-overview
        ["zhipu"] =
        [
            new("glm-5.3", "GLM-5.3"),
            new("glm-5.3-flash", "GLM-5.3 Flash"),
            new("glm-5.3-flashx", "GLM-5.3 FlashX"),
            new("glm-5.2", "GLM-5.2"),
            new("glm-5.1", "GLM-5.1"),
            new("glm-5-turbo", "GLM-5 Turbo"),
            new("glm-5", "GLM-5"),
            new("glm-5v-turbo", "GLM-5v Turbo"),
            new("glm-4.7", "GLM-4.7"),
            new("glm-4.7-flash", "GLM-4.7 Flash"),
            new("glm-4.7-flashx", "GLM-4.7 FlashX"),
            new("glm-4.6", "GLM-4.6"),
            new("glm-4.6v", "GLM-4.6v"),
            new("glm-4.6v-flash", "GLM-4.6v Flash"),
            new("glm-4.6v-flashx", "GLM-4.6v FlashX"),
            new("glm-4.5-air", "GLM-4.5 Air"),
            new("glm-4.5-airx", "GLM-4.5 AirX"),
            new("glm-4.5-flash", "GLM-4.5 Flash"),
            new("glm-4.1v-thinking-flash", "GLM-4.1v Thinking Flash"),
            new("glm-4.1v-thinking-flashx", "GLM-4.1v Thinking FlashX"),
            new("glm-4v-flash", "GLM-4v Flash"),
            new("glm-4-flash-250414", "GLM-4 Flash 250414"),
            new("glm-4-flashx-250414", "GLM-4 FlashX 250414"),
            new("glm-4-long", "GLM-4 Long"),
            new("glm-4-plus", "GLM-4 Plus"),
        ],
        // https://platform.minimax.io/docs/api-reference/text-chat-openai
        ["minimax"] =
        [
            new("MiniMax-M3", "MiniMax M3"),
            new("MiniMax-M2.7", "MiniMax M2.7"),
            new("MiniMax-M2.7-highspeed", "MiniMax M2.7 Highspeed"),
            new("MiniMax-M2.5", "MiniMax M2.5"),
            new("MiniMax-M2.5-highspeed", "MiniMax M2.5 Highspeed"),
            new("MiniMax-M2.1", "MiniMax M2.1"),
            new("MiniMax-M2.1-highspeed", "MiniMax M2.1 Highspeed"),
            new("MiniMax-M2", "MiniMax M2"),
        ],
        // https://docs.x.ai/developers/models
        ["xai"] =
        [
            new("grok-4.7", "Grok 4.7"),
            new("grok-4.6", "Grok 4.6"),
            new("grok-4.5", "Grok 4.5"),
            new("grok-4.3", "Grok 4.3"),
            new("grok-4.20-0309-reasoning", "Grok 4.20 Reasoning"),
            new("grok-4.20-0309-non-reasoning", "Grok 4.20 Non-reasoning"),
            new("grok-build-0.1", "Grok Build 0.1"),
            new("grok-4.20-multi-agent", "Grok 4.20 Multi-agent"),
        ],
        ["local"] = [new("qwen3:8b", "Qwen3 8B")]
    };

    private static readonly IReadOnlyDictionary<string, ModelDetail> ById = ByProvider.Values
        .SelectMany(models => models).ToDictionary(model => model.Id, StringComparer.Ordinal);

    public static string[] Ids(string providerId) => ByProvider[providerId].Select(model => model.Id).ToArray();

    public static ModelDetail Describe(string id) => ById.TryGetValue(id, out var model) ? model : new(id, id);
}

