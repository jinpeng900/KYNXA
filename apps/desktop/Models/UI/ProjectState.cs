namespace KYNXA_Desktop.Models.UI;

public sealed class ProjectState
{
    public Guid Id { get; set; } = Guid.NewGuid();
    public string Name { get; set; } = string.Empty;
    public string? FolderPath { get; set; }
    public bool IsPinned { get; set; }
    public bool IsArchived { get; set; }
    public bool IsFolderlessWorkspace { get; set; }
    public List<ProjectChatState> Chats { get; set; } = [];
}

public sealed class ProjectChatState
{
    public Guid Id { get; set; } = Guid.NewGuid();
    public string Title { get; set; } = "新聊天";
    public string Draft { get; set; } = string.Empty;
    public bool IsSample { get; set; }
    public bool IsPinned { get; set; }
    public bool IsArchived { get; set; }
    public List<ChatMessageState> Messages { get; set; } = [];
    [System.Text.Json.Serialization.JsonIgnore]
    public bool CanPersist => IsSample || Messages.Count > 0;
}
