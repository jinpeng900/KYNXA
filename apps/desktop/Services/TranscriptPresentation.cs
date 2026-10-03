using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

/// <summary>Visible conversation content; the durable transcript and model history remain untouched.</summary>
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
