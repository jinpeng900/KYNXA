using System.Diagnostics;
using System.Reflection;
using System.Text;
using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Views;

namespace KYNXA_Desktop;

public partial class App
{
    private void CheckReplyTextSnapshots(object pending)
    {
        var type = pending.GetType();
        var message = (ChatMessageState)type.GetProperty("Message")!.GetValue(pending)!;
        var content = (StringBuilder)type.GetProperty("Content")!.GetValue(pending)!;
        var reasoning = (StringBuilder)type.GetProperty("Reasoning")!.GetValue(pending)!;
        var segmentText = (Dictionary<string, (StringBuilder Content, StringBuilder Reasoning)>)
            type.GetProperty("SegmentText")!.GetValue(pending)!;
        var dirtySegments = (HashSet<string>)type.GetProperty("DirtySegments")!.GetValue(pending)!;
        var acceptSnapshot = typeof(ShellPage).GetMethod("AcceptReplyTextSnapshot", BindingFlags.NonPublic | BindingFlags.Static)!;
        string originalContent = message.Content, originalReasoning = message.Reasoning;
        try
        {
            content.Append(new string('文', 140000)); reasoning.Append(new string('思', 20000));
            Call("FlushReply", pending, true);
            string contentSnapshot = message.Content, reasoningSnapshot = message.Reasoning;
            type.GetProperty("Dirty")!.SetValue(pending, true);
            Call("FlushReply", pending, false);
            Check(ReferenceEquals(contentSnapshot, message.Content) && ReferenceEquals(reasoningSnapshot, message.Reasoning),
                "timing and tool-only refreshes reuse both long text snapshots without copying them");
            content.Append("FINAL_TAIL");
            type.GetProperty("Dirty")!.SetValue(pending, true);
            type.GetProperty("LastTextFlushTimestamp")!.SetValue(pending, Stopwatch.GetTimestamp());
            Call("FlushReply", pending, false);
            Check(ReferenceEquals(contentSnapshot, message.Content) && (bool)type.GetProperty("Dirty")!.GetValue(pending)!,
                "large active text is coalesced without discarding its pending tail");
            Call("FlushReply", pending, true);
            Check(message.Content == content.ToString() && message.Content.EndsWith("FINAL_TAIL", StringComparison.Ordinal)
                && ReferenceEquals(reasoningSnapshot, message.Reasoning),
                "final or pause flush bypasses the interval and preserves all text without recopying unchanged reasoning");
            acceptSnapshot.Invoke(null, [pending, "AAAA", "1111"]);
            Call("FlushReply", pending, true);
            acceptSnapshot.Invoke(null, [pending, "BBBB", "2222"]);
            Call("FlushReply", pending, true);
            Check(message.Content == "BBBB" && message.Reasoning == "2222",
                "same-length absolute reconnect snapshots replace previous content and reasoning");

            var segment = new AssistantSegment("snapshot-channel", 1, 1, "commentary", "streaming", "PART", new string('思', 1000));
            message.AssistantSegments.Add(segment);
            segmentText[segment.Id] = (new(segment.Content), new(segment.Reasoning));
            segmentText[segment.Id].Content.Append("_TAIL"); dirtySegments.Add(segment.Id);
            Call("FlushReply", pending, true);
            Check(message.AssistantSegments[^1].Content == "PART_TAIL"
                && ReferenceEquals(segment.Reasoning, message.AssistantSegments[^1].Reasoning),
                "segment growth retains complete Markdown source and reuses the unchanged reasoning channel");
        }
        finally
        {
            acceptSnapshot.Invoke(null, [pending, originalContent, originalReasoning]);
            message.AssistantSegments.Clear(); segmentText.Clear(); dirtySegments.Clear();
            type.GetProperty("LastTextFlushTimestamp")!.SetValue(pending, 0L);
            Call("FlushReply", pending, true);
        }
    }
}
