using System.Text.Json;
using KYNXA_Desktop.Models.UI;

namespace KYNXA_Desktop.Services;

/// <summary>Local project metadata; linked workspace files are never changed by catalog actions.</summary>
public sealed class ProjectStore(string dataDirectory)
{
    private readonly string _catalogPath = Path.Combine(dataDirectory, "projects.json");
    private readonly string _chatPath = Path.Combine(dataDirectory, "chats.json");
    private static readonly JsonSerializerOptions JsonOptions = new() { WriteIndented = true };

    public bool Exists => File.Exists(_catalogPath);
    public bool ChatsExist => File.Exists(_chatPath);

    public List<ProjectState> Load()
    {
        if (!Exists) return [];
        var projects = JsonSerializer.Deserialize<List<ProjectState>>(File.ReadAllText(_catalogPath))
            ?? throw new InvalidDataException("项目列表为空或格式无效。");
        if (projects.Any(p => p is null || p.Id == Guid.Empty || string.IsNullOrWhiteSpace(p.Name) ||
                p.Chats is null || p.Chats.Any(c => c is null || c.Id == Guid.Empty || string.IsNullOrWhiteSpace(c.Title))) ||
            projects.Select(p => p.Id).Distinct().Count() != projects.Count ||
            projects.SelectMany(p => p.Chats).Select(c => c.Id).Distinct().Count() != projects.Sum(p => p.Chats.Count))
            throw new InvalidDataException("项目列表格式无效，原文件已保留。");
        return projects;
    }

    public void Save(IReadOnlyList<ProjectState> projects)
    {
        // Preserve typed drafts; only untouched or whitespace-only new threads are omitted.
        var saved = projects.Select(project => new ProjectState
        {
            Id = project.Id, Name = project.Name, FolderPath = project.FolderPath,
            IsPinned = project.IsPinned, IsArchived = project.IsArchived, IsFolderlessWorkspace = project.IsFolderlessWorkspace,
            Chats = project.Chats.Where(chat => chat.CanPersist).ToList()
        });
        WriteAtomically(_catalogPath, saved);
    }

    public List<ProjectChatState> LoadChats() => !ChatsExist ? [] :
        JsonSerializer.Deserialize<List<ProjectChatState>>(File.ReadAllText(_chatPath))
        ?? throw new InvalidDataException("聊天列表格式无效，原文件已保留。");

    public void SaveChats(IEnumerable<ProjectChatState> chats) => WriteAtomically(_chatPath, chats.Where(chat => chat.CanPersist));

    private void WriteAtomically<T>(string path, T value)
    {
        Directory.CreateDirectory(dataDirectory);
        string temporary = path + ".tmp";
        File.WriteAllText(temporary, JsonSerializer.Serialize(value, JsonOptions));
        File.Move(temporary, path, overwrite: true);
    }

    public string CreateManagedFolder(Guid projectId)
    {
        string folder = Path.Combine(dataDirectory, "Projects", projectId.ToString("N"));
        Directory.CreateDirectory(folder);
        return folder;
    }
}
