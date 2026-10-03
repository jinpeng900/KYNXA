using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

/// <summary>Visible conversation content; the durable transcript and model history remain untouched.</summary>
internal static class TranscriptPresentation
{
    internal const int RecentSegmentLimit = 3;
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
            if (ordered.Length == 0)
                return new(string.IsNullOrWhiteSpace(content) ? "incomplete" : "final", content, [], []);

            var final = ordered.LastOrDefault(segment => segment.Phase == "final_answer"
                && segment.Status == "completed" && !string.IsNullOrWhiteSpace(segment.Content));
            if (final is not null) return new("final", final.Content, [final], []);

            // A recorded phase is not proof that the enclosing request finished successfully.
            return Partial("incomplete", ordered, orderedTools, "");
        }

        if (status != "streaming")
            return Partial("partial", ordered, orderedTools, ordered.Length == 0 ? content : "");

        var approvalRounds = orderedTools.Where(RequiresApproval).Select(tool => tool.Round).ToHashSet();
        var toolRounds = orderedTools.Select(tool => tool.Round).ToHashSet();
        var latest = ordered.LastOrDefault();
        var candidates = ordered.Where(segment => !string.IsNullOrWhiteSpace(segment.Content)
            || toolRounds.Contains(segment.Round)
            || (segment == latest && segment.Status == "streaming")).ToArray();
        var recentIds = candidates.TakeLast(RecentSegmentLimit).Select(segment => segment.Id).ToHashSet(StringComparer.Ordinal);
        var visible = ordered.Where(segment => recentIds.Contains(segment.Id) || approvalRounds.Contains(segment.Round)).ToArray();
        var visibleRounds = visible.Select(segment => segment.Round).ToHashSet();
        var visibleTools = RecentTools(orderedTools.Where(tool => tool.Round is null || visibleRounds.Contains(tool.Round.Value)
            || RequiresApproval(tool)).ToArray());
        string visibleContent = ordered.Length == 0 ? content
            : string.Join("\n\n", visible.Select(segment => segment.Content).Where(text => !string.IsNullOrWhiteSpace(text)));
        return new("active", visibleContent, visible, visibleTools);
    }

    private static VisibleMessage Partial(string mode, AssistantSegment[] segments, IReadOnlyList<ToolActivity> tools, string fallback)
    {
        var lastText = segments.LastOrDefault(segment => !string.IsNullOrWhiteSpace(segment.Content));
        return new(mode, lastText?.Content ?? fallback, lastText is null ? [] : [lastText], tools.Where(RequiresApproval).ToArray());
    }

    private static ToolActivity[] RecentTools(IReadOnlyList<ToolActivity> tools)
    {
        var recent = tools.Where(tool => !RequiresApproval(tool)).TakeLast(RecentToolLimit)
            .Select(tool => tool.ToolCallId).ToHashSet(StringComparer.Ordinal);
        return tools.Where(tool => RequiresApproval(tool) || recent.Contains(tool.ToolCallId)).ToArray();
    }

    private static bool RequiresApproval(ToolActivity tool) => tool.Status == "approval-required";
}
