using System.Text;
using KYNXA_Desktop.Models.UI;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Produces a readable Markdown snapshot using the same visible content as the main transcript.
/// 使用与主聊天相同的可见正文生成可读 Markdown 快照。
/// </summary>
internal static class ConversationExport
{
    internal static string ToMarkdown(string title, IReadOnlyList<ChatMessageState> messages, string userLabel, string assistantLabel)
    {
        var blocks = new List<(string Role, string Content)>();
        foreach (var message in messages)
        {
            if (message.Role is not ("user" or "assistant")) continue;
            var visible = TranscriptPresentation.Select(message.Role, message.Status, message.Content,
                message.AssistantSegments, message.ToolActivities);
            if (!string.IsNullOrWhiteSpace(visible.Content)) blocks.Add((message.Role, visible.Content));
        }
        if (blocks.Count == 0) return string.Empty;

        var markdown = new StringBuilder();
        if (!string.IsNullOrWhiteSpace(title)) markdown.Append("# ").AppendLine(HeadingText(title)).AppendLine();
        foreach (var block in blocks)
        {
            markdown.Append("## ").AppendLine(HeadingText(block.Role == "user" ? userLabel : assistantLabel)).AppendLine();
            // Keep raw Markdown, TeX and code indentation. Hidden reasoning/tool receipts stay in the gateway archive.
            // 保留原 Markdown、TeX 与代码缩进；隐藏的思考和工具回执仍保留在网关归档中。
            markdown.Append(block.Content).AppendLine().AppendLine();
        }
        return markdown.ToString();
    }

    private static string HeadingText(string value)
    {
        string singleLine = value.Replace('\r', ' ').Replace('\n', ' ').Trim();
        foreach (char character in "\\`*_{}[]<>()#+-.!|") singleLine = singleLine.Replace(character.ToString(), "\\" + character);
        return singleLine;
    }
}
