using System.Net.Http.Json;
using System.Text.Json;
using KYNXA_Desktop.Models.UI;

namespace KYNXA_Desktop.Services;

public sealed record ConversationCatalog(int Revision, List<ProjectState> Projects, List<ProjectChatState> Chats);

/// <summary>
/// The gateway owns conversation persistence. The desktop keeps only its current UI projection.
/// 网关拥有正式聊天持久化；桌面只保留当前展示投影。
/// </summary>
public sealed class ProjectStore(string dataDirectory) : IDisposable
{
    private readonly HttpClient _httpClient = new() { BaseAddress = ModelGatewayService.Address, Timeout = TimeSpan.FromSeconds(30) };
    private readonly SemaphoreSlim _requestGate = new(1, 1);
    private static readonly JsonSerializerOptions JsonOptions = new() { PropertyNameCaseInsensitive = true };
    private int? _revision;
    private readonly HashSet<(Guid Chat, Guid Message)> _knownUserMessages = [];
    private readonly object _knownUserMessagesLock = new();

    public async Task<ConversationCatalog> LoadAsync(CancellationToken cancellationToken = default)
    {
        await _requestGate.WaitAsync(cancellationToken);
        try
        {
            await ModelGatewayService.EnsureReadyAsync(cancellationToken);
            using var response = await _httpClient.GetAsync("/api/conversations/catalog", cancellationToken);
            var catalog = await ReadAsync(response, cancellationToken);
            AcceptCatalog(catalog);
            return catalog;
        }
        finally { _requestGate.Release(); }
    }

    // Capture snapshots before the first await: UI collections can change during a request.
    // 首个 await 前捕获快照；请求执行时 UI 集合可能继续变化。
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
        lock (_knownUserMessagesLock)
            return new()
            {
                Id = chat.Id, Title = chat.Title, Draft = chat.Draft, IsSample = chat.IsSample,
                IsPinned = chat.IsPinned, IsArchived = chat.IsArchived,
                // Runtime owns assistant content. Metadata updates send only new submitted user messages.
                // 助手内容由运行时负责；元数据更新只发送新提交的用户消息。
                Messages = chat.Messages.Where(message => message.Role == "user" && !_knownUserMessages.Contains((chat.Id, message.Id))).ToList()
            };
    }

    private void AcceptCatalog(ConversationCatalog catalog)
    {
        _revision = catalog.Revision;
        lock (_knownUserMessagesLock)
            foreach (var chat in catalog.Chats.Concat(catalog.Projects.SelectMany(project => project.Chats)))
                foreach (var message in chat.Messages.Where(message => message.Role == "user"))
                    _knownUserMessages.Add((chat.Id, message.Id));
    }

    private async Task SaveSnapshotAsync(JsonElement? projects, JsonElement? chats)
    {
        await _requestGate.WaitAsync();
        try
        {
            await ModelGatewayService.EnsureReadyAsync();
            if (_revision is null) throw new InvalidOperationException(UiText.Get("会话目录尚未加载，请重新打开 KYNXA。"));
            var payload = new Dictionary<string, object> { ["Revision"] = _revision.Value };
            if (projects is { } projectData) payload["Projects"] = projectData;
            if (chats is { } chatData) payload["Chats"] = chatData;
            using var response = await _httpClient.PutAsJsonAsync("/api/conversations/catalog", payload, JsonOptions);
            var catalog = await ReadAsync(response, CancellationToken.None);
            AcceptCatalog(catalog);
        }
        finally { _requestGate.Release(); }
    }

    private static Task<ConversationCatalog> ReadAsync(HttpResponseMessage response, CancellationToken cancellationToken) =>
        GatewayResponseReader.ReadAsync<ConversationCatalog>(response, UiText.Get("会话存储接口返回了空目录。"),
            UiText.Get("会话存储接口返回 HTTP {0}。"), cancellationToken, JsonOptions,
            UiText.Get("聊天目录已在其他窗口中更新，请重新打开 KYNXA 后重试。"));

    public string CreateManagedFolder(Guid projectId)
    {
        string folder = Path.Combine(dataDirectory, "Projects", projectId.ToString("N"));
        Directory.CreateDirectory(folder);
        return folder;
    }

    public void Dispose() => _httpClient.Dispose();
}
