using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Visible conversation content; the durable transcript and model history remain untouched.
/// 聊天可见内容的投影；正式聊天记录与模型历史不变。
/// </summary>
internal static class TranscriptPresentation
{
    internal const int RecentToolLimit = 8;

    internal sealed record VisibleMessage(string Mode, string Content, AssistantSegment[] Segments, ToolActivity[] Tools);

    internal static VisibleMessage Select(string role, string status, string content,
        IReadOnlyList<AssistantSegment> segments, IReadOnlyList<ToolActivity> tools)
    {
        if (role == "user") return new("user", content, [], []);

        var ordered = segments.OrderBy(segment => segment.Order).ToArray();
        var orderedTools = tools.Select((tool, index) => (Tool: tool, Order: tool.Order ?? index))
            .OrderBy(item => item.Order).Select(item => item.Tool).ToArray();
        if (status == "completed")
        {
            if (ordered.Length == 0 && !string.IsNullOrWhiteSpace(content))
                return new("final", content, [], []);

            var final = ordered.LastOrDefault(segment => segment.Phase == "final_answer"
                && segment.Status == "completed" && !string.IsNullOrWhiteSpace(segment.Content));
            if (final is not null) return new("final", final.Content, [final], []);

            // A recorded phase is not proof that the enclosing request finished successfully.
            // 存在阶段记录不代表整个请求已成功完成。
            return Process("incomplete", ordered, orderedTools, ordered.Length == 0 ? content : "");
        }

        if (status != "streaming")
            return Process("partial", ordered, orderedTools, ordered.Length == 0 ? content : "");

        return Process("active", ordered, orderedTools, ordered.Length == 0 ? content : "");
    }

    private static VisibleMessage Process(string mode, AssistantSegment[] segments, IReadOnlyList<ToolActivity> tools, string fallback)
    {
        var visibleTools = RecentTools(tools);
        var toolRounds = visibleTools.Select(tool => tool.Round).ToHashSet();
        var latest = segments.LastOrDefault();
        // Spoken progress stays readable until a successful final answer replaces it.
        // 成功的最终回答替换中间进展之前，所有已输出的话仍可阅读。
        var visible = segments.Where(segment => !string.IsNullOrWhiteSpace(segment.Content)
            || toolRounds.Contains(segment.Round)
            || (mode == "active" && segment == latest && segment.Status == "streaming")).ToArray();
        var content = segments.Length == 0 ? fallback
            : string.Join("\n\n", visible.Select(segment => segment.Content).Where(text => !string.IsNullOrWhiteSpace(text)));
        return new(mode, content, visible, visibleTools);
    }

    private static ToolActivity[] RecentTools(IReadOnlyList<ToolActivity> tools)
    {
        var recent = tools.Where(tool => !RequiresApproval(tool)).TakeLast(RecentToolLimit)
            .Select(tool => tool.ToolCallId).ToHashSet(StringComparer.Ordinal);
        return tools.Where(tool => RequiresApproval(tool) || recent.Contains(tool.ToolCallId)).ToArray();
    }

    private static bool RequiresApproval(ToolActivity tool) => tool.Status == "approval-required";
}
