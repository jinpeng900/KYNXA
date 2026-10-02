using System.Net;
using System.Net.Http.Json;
using System.Text.Json;
using KYNXA_Desktop.Models.UI;

namespace KYNXA_Desktop.Services;

public sealed record ConversationCatalog(int Revision, List<ProjectState> Projects, List<ProjectChatState> Chats);

/// <summary>The gateway owns conversation persistence. The desktop keeps only its current UI projection.</summary>
public sealed class ProjectStore(string dataDirectory) : IDisposable
{
    private readonly HttpClient _http = new() { BaseAddress = ModelGatewayService.Address, Timeout = TimeSpan.FromSeconds(30) };
    private readonly SemaphoreSlim _requests = new(1, 1);
    private static readonly JsonSerializerOptions JsonOptions = new() { PropertyNameCaseInsensitive = true };
    private int? _revision;
    private readonly HashSet<(Guid Chat, Guid Message)> _knownUsers = [];
    private readonly object _knownUsersLock = new();

    public async Task<ConversationCatalog> LoadAsync(CancellationToken cancellationToken = default)
    {
        await _requests.WaitAsync(cancellationToken);
        try
        {
            await ModelGatewayService.EnsureReadyAsync(cancellationToken);
            using var response = await _http.GetAsync("/api/conversations/catalog", cancellationToken);
            var catalog = await ReadAsync(response, cancellationToken);
            AcceptCatalog(catalog);
            return catalog;
        }
        finally { _requests.Release(); }
    }

    // Capture snapshots before the first await: UI collections can change during a request.
    public Task SaveAsync(IReadOnlyList<ProjectState> projects) => SaveSnapshotAsync(
        JsonSerializer.SerializeToElement(projects.Select(project => new ProjectState
        {
            Id = project.Id, Name = project.Name, FolderPath = project.FolderPath,
            IsPinned = project.IsPinned, IsArchived = project.IsArchived, IsFolderlessWorkspace = project.IsFolderlessWorkspace,
            Chats = project.Chats.Where(chat => chat.CanPersist).Select(SnapshotChat).ToList()
        }), JsonOptions), null);

    public Task SaveChatsAsync(IEnumerable<ProjectChatState> chats) => SaveSnapshotAsync(null,
        JsonSerializer.SerializeToElement(chats.Where(chat => chat.CanPersist).Select(SnapshotChat), JsonOptions));

    private ProjectChatState SnapshotChat(ProjectChatState chat)
    {
        lock (_knownUsersLock)
            return new()
            {
                Id = chat.Id, Title = chat.Title, Draft = chat.Draft, IsSample = chat.IsSample,
                IsPinned = chat.IsPinned, IsArchived = chat.IsArchived,
                // Runtime owns assistant content. Metadata updates send only new submitted user messages.
                Messages = chat.Messages.Where(message => message.Role == "user" && !_knownUsers.Contains((chat.Id, message.Id))).ToList()
            };
    }

    private void AcceptCatalog(ConversationCatalog catalog)
    {
        _revision = catalog.Revision;
        lock (_knownUsersLock)
            foreach (var chat in catalog.Chats.Concat(catalog.Projects.SelectMany(project => project.Chats)))
                foreach (var message in chat.Messages.Where(message => message.Role == "user"))
                    _knownUsers.Add((chat.Id, message.Id));
    }

    private async Task SaveSnapshotAsync(JsonElement? projects, JsonElement? chats)
    {
        await _requests.WaitAsync();
        try
        {
            await ModelGatewayService.EnsureReadyAsync();
            if (_revision is null) throw new InvalidOperationException("会话目录尚未加载，请重新打开 KYNXA。");
            var payload = new Dictionary<string, object> { ["Revision"] = _revision.Value };
            if (projects is { } projectData) payload["Projects"] = projectData;
            if (chats is { } chatData) payload["Chats"] = chatData;
            using var response = await _http.PutAsJsonAsync("/api/conversations/catalog", payload, JsonOptions);
            var catalog = await ReadAsync(response, CancellationToken.None);
            AcceptCatalog(catalog);
        }
        finally { _requests.Release(); }
    }

    private static async Task<ConversationCatalog> ReadAsync(HttpResponseMessage response, CancellationToken cancellationToken)
    {
        if (!response.IsSuccessStatusCode)
        {
            if (response.StatusCode == HttpStatusCode.Conflict)
                throw new InvalidOperationException("聊天目录已在其他窗口中更新，请重新打开 KYNXA 后重试。");
            string? error = null;
            try
            {
                using var body = await response.Content.ReadFromJsonAsync<JsonDocument>(cancellationToken);
                if (body?.RootElement.TryGetProperty("error", out var field) == true) error = field.GetString();
            }
            catch (JsonException) { }
            throw new InvalidOperationException(error ?? $"会话存储接口返回 HTTP {(int)response.StatusCode}。");
        }
        return await response.Content.ReadFromJsonAsync<ConversationCatalog>(JsonOptions, cancellationToken)
            ?? throw new InvalidDataException("会话存储接口返回了空目录。");
    }

    public string CreateManagedFolder(Guid projectId)
    {
        string folder = Path.Combine(dataDirectory, "Projects", projectId.ToString("N"));
        Directory.CreateDirectory(folder);
        return folder;
    }

    public void Dispose() => _http.Dispose();
}
