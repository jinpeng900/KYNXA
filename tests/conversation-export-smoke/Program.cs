using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;

int checks = 0;
void Check(bool condition, string description)
{
    if (!condition) throw new InvalidOperationException(description);
    checks++;
}

const string rawUser = "第一行\n  第二行\n\n$\\frac{1}{2}$";
const string rawFinal = "答案\n\n```cs\n    int count = 2;\n```\n\n$$\\alpha^2$$";
var user = new ChatMessageState { Role = "user", Content = rawUser };
var completed = new ChatMessageState
{
    Role = "assistant", Content = "outdated aggregate", Reasoning = "private reasoning",
    AssistantSegments =
    [
        new("stage", 1, 1, "commentary", "completed", "HIDDEN_SUCCESS_STAGE", "HIDDEN_THOUGHT"),
        new("answer", 2, 2, "final_answer", "completed", rawFinal, "HIDDEN_FINAL_THOUGHT")
    ],
    ToolActivities = [new("tool", "read_file", null, "completed", "HIDDEN_SUMMARY", "HIDDEN_TOOL_RECEIPT", Round: 1, Order: 1)]
};
string completeExport = ConversationExport.ToMarkdown("Title\n# injected", [user, completed], "用户", "助手");
Check(completeExport.Contains(rawUser) && completeExport.Contains(rawFinal), "Export preserves raw user whitespace, code indentation and TeX.");
Check(!completeExport.Contains("HIDDEN_") && !completeExport.Contains("outdated aggregate") && !completeExport.Contains("private reasoning"),
    "Successful export follows the visible final answer and excludes hidden execution data.");
Check(completeExport.StartsWith("# Title \\# injected"), "Multiline titles cannot create extra Markdown headings.");
Check(completeExport.IndexOf("## 用户", StringComparison.Ordinal) < completeExport.IndexOf("## 助手", StringComparison.Ordinal),
    "Export keeps message order and supplied localized labels.");
Check(completed.AssistantSegments.Count == 2 && completed.ToolActivities.Count == 1 && completed.Content == "outdated aggregate",
    "A display export does not modify formal source content or trace records.");
completed.Status = "streaming";
string activeExport = ConversationExport.ToMarkdown("", [completed], "User", "Assistant");
Check(activeExport.Contains("HIDDEN_SUCCESS_STAGE") && activeExport.Contains(rawFinal),
    "A completed final phase does not hide spoken stages until the enclosing request completes.");
Check(!activeExport.Contains("HIDDEN_THOUGHT") && !activeExport.Contains("HIDDEN_TOOL_RECEIPT"),
    "Active export omits reasoning and JSON receipts while preserving visible stages.");
completed.Status = "completed";

var interrupted = new ChatMessageState
{
    Role = "assistant", Status = "interrupted", Content = "STALE_FINAL_ONLY",
    AssistantSegments =
    [
        new("earlier", 1, 1, "commentary", "completed", "Earlier spoken stage", "hidden"),
        new("partial", 2, 2, "final_answer", "interrupted", "Partial final $x$", "hidden")
    ]
};
string interruptedExport = ConversationExport.ToMarkdown("", [interrupted], "User", "Assistant");
Check(interruptedExport.Contains("Earlier spoken stage\n\nPartial final $x$"), "An interrupted request keeps all visible spoken stages.");
Check(!interruptedExport.Contains("STALE_FINAL_ONLY") && !interruptedExport.Contains("hidden"), "Interrupted export does not use stale aggregate or reasoning.");
Check(interruptedExport.StartsWith("## Assistant"), "Empty titles omit the document heading.");
var legacy = new ChatMessageState { Role = "assistant", Content = "Legacy answer" };
Check(ConversationExport.ToMarkdown("", [legacy], "User", "Assistant").Contains("Legacy answer"), "Legacy completed messages remain compatible.");
var incomplete = new ChatMessageState
{
    Role = "assistant", Content = "not proven final",
    AssistantSegments = [new("stage", 1, 1, "commentary", "completed", "Spoken stage", "hidden")]
};
Check(ConversationExport.ToMarkdown("", [incomplete], "User", "Assistant").Contains("Spoken stage"), "Missing final answer retains the visible incomplete projection.");
Check(ConversationExport.ToMarkdown("Empty", [], "User", "Assistant") == "", "Empty conversations produce no export content.");
Check(ConversationExport.ToMarkdown("Empty", [new() { Role = "system", Content = "INTERNAL" }], "User", "Assistant") == "",
    "Internal roles do not become visible chat export.");
Console.WriteLine($"Conversation export smoke: {checks} checks passed.");
