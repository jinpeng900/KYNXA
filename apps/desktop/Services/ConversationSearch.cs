using KYNXA_Desktop.Models.UI;

namespace KYNXA_Desktop.Services;

/// <summary>Filters existing local history without changing its order or stored contents.</summary>
public static class ConversationSearch
{
    public static bool MatchesChat(ProjectChatState chat, string query, string? projectName = null)
    {
        if (chat.IsArchived) return false;
        string term = query.Trim();
        if (term.Length == 0) return true;

        return Contains(chat.Title, term)
            || Contains(projectName, term)
            || chat.Messages.Any(message => Contains(message.Content, term));
    }

    public static bool MatchesProject(ProjectState project, string query)
    {
        if (project.IsArchived) return false;
        string term = query.Trim();
        if (term.Length == 0) return true;

        return Contains(project.Name, term)
            || project.Chats.Any(chat => MatchesChat(chat, term));
    }

    private static bool Contains(string? text, string term) =>
        text?.Contains(term, StringComparison.OrdinalIgnoreCase) == true;
}
