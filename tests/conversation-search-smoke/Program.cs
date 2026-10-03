using System.Text.Json;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;

static void Check(bool condition, string message)
{
    if (!condition) throw new InvalidOperationException(message);
}

var codeChat = new ProjectChatState
{
    Title = "分析模型网关",
    IsPinned = true,
    Draft = "尚未发送的草稿",
    Messages =
    [
        new() { Role = "user", Content = "请解释 API 调用中的异常处理" },
        new() { Role = "assistant", Content = "使用 CancellationToken 可以停止本地等待。\n```csharp\nawait SendAsync();\n```" }
    ]
};
var learningChat = new ProjectChatState
{
    Title = "软件工程学习",
    Messages = [new() { Content = "先画流程图，再讨论模块边界。" }]
};
var archivedChat = new ProjectChatState
{
    Title = "归档的模型网关",
    IsArchived = true,
    Messages = [new() { Content = "只有这里有隐藏记录关键词。" }]
};
var emptyChat = new ProjectChatState { Title = "新聊天" };
var project = new ProjectState
{
    Name = "课设 KYNXA",
    IsPinned = true,
    Chats = [codeChat, archivedChat, learningChat, emptyChat]
};
var secondProject = new ProjectState { Name = "阅读计划", Chats = [new() { Title = "复习软件工程" }] };
var archivedProject = new ProjectState
{
    Name = "归档课设 KYNXA",
    IsArchived = true,
    Chats = [new() { Title = "分析模型网关" }]
};
var projects = new[] { project, archivedProject, secondProject };
string before = JsonSerializer.Serialize(projects);

Check(ConversationSearch.MatchesChat(codeChat, "模型网关"), "A Chinese title must be searchable.");
Check(ConversationSearch.MatchesChat(learningChat, "模块边界"), "A Chinese message must be searchable even when the title does not match.");
Check(ConversationSearch.MatchesChat(codeChat, "  cancellationtoken  "), "English message search must ignore case and trim the query.");
Check(ConversationSearch.MatchesChat(codeChat, "SENDASYNC"), "Search must cover the original code content in a reply.");
Check(ConversationSearch.MatchesChat(emptyChat, "kynxa", project.Name), "A chat must match its supplied project name.");
Check(ConversationSearch.MatchesChat(emptyChat, " \t\r\n "), "A blank query must retain an empty active chat.");
Check(!ConversationSearch.MatchesChat(codeChat, "尚未发送"), "Search must not expose an unsent draft as a conversation result.");
Check(!ConversationSearch.MatchesChat(codeChat, ".*"), "Search input must be treated as literal text rather than a regular expression.");
Check(!ConversationSearch.MatchesChat(codeChat, "找不到的内容"), "An unrelated query must not match.");
Check(!ConversationSearch.MatchesChat(archivedChat, "模型网关"), "An archived chat must stay excluded even when its title matches.");
Check(!ConversationSearch.MatchesChat(archivedChat, ""), "Clearing the query must not reveal archived chats.");

Check(ConversationSearch.MatchesProject(project, "kYnXa"), "Project names must be searchable without case sensitivity.");
Check(ConversationSearch.MatchesProject(project, "流程图"), "A project must remain visible when an active chat message matches.");
Check(!ConversationSearch.MatchesProject(project, "隐藏记录关键词"), "An archived child must not make a project match.");
Check(ConversationSearch.MatchesProject(new ProjectState { Name = "空项目" }, ""), "A blank query must retain an active empty project.");
Check(!ConversationSearch.MatchesProject(archivedProject, "kynxa"), "An archived project must stay excluded when its name matches.");
Check(!ConversationSearch.MatchesProject(archivedProject, ""), "Clearing the query must not reveal archived projects.");

var visibleChats = project.Chats.Where(chat => ConversationSearch.MatchesChat(chat, "工程")).ToArray();
Check(visibleChats.SequenceEqual(new[] { learningChat }), "Only matching active chats should survive a filtered view.");
var restoredChats = project.Chats.Where(chat => ConversationSearch.MatchesChat(chat, "")).ToArray();
Check(restoredChats.SequenceEqual(new[] { codeChat, learningChat, emptyChat }), "Clearing a search must preserve the original pinned and chat order.");
var visibleProjects = projects.Where(candidate => ConversationSearch.MatchesProject(candidate, "工程")).ToArray();
Check(visibleProjects.SequenceEqual(new[] { project, secondProject }), "Project filtering must retain the existing order while excluding archived projects.");
Check(!projects.Any(candidate => ConversationSearch.MatchesProject(candidate, "没有这条记录")), "A query with no results must produce an empty view.");
Check(JsonSerializer.Serialize(projects) == before, "Searching must not change stored history, pin flags, drafts, or ordering.");

Console.WriteLine("PASS: local conversation search, archive exclusion, stable filtering, and unchanged history.");
