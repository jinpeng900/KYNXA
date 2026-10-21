using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private readonly Dictionary<Guid, Guid> _workspaceMountSelections = [];
    private Guid? _filesConversation;
    private Guid? _filesMount;
    private string? _filesBinding;
    private long _filesGeneration;
    private string? _filesRootDirectory;
    private string? _filesPath;
    private int _attachmentOffset;
    private CancellationTokenSource? _workspaceFileRead;
    private string? _retrievalActivationKey;

    private void NotifyActiveRetrievalProject(ProjectState? project)
    {
        if (!_projectsReady || _chatClosing || _projectViewClosed) return;
        string key = project is null ? "global" : $"{project.Id:D}/{project.FolderPath}/" +
            string.Join(";", project.Mounts.Select(mount => $"{mount.Id:D}:{mount.BindingRevision}:{mount.Path}"));
        if (_retrievalActivationKey == key) return;
        _retrievalActivationKey = key;
        _ = ActivateRetrievalProjectAsync(project is { Mounts.Count: > 0 } ? project.Id : null);
    }

    private async Task ActivateRetrievalProjectAsync(Guid? projectId)
    {
        try
        {
            using var api = new RetrievalApiClient();
            await api.ActivateAsync(projectId, _historyLifetime.Token);
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException or InvalidOperationException or InvalidDataException)
        {
            // Activation is a background hint; retrieval still prepares itself when a real query needs it.
            // 激活只是后台提示，失败不阻断聊天；真正查询仍由后端按需准备检索。
        }
    }

    private ProjectState? CurrentWorkspaceProject() => ActiveChatId is Guid conversation ? FindChat(conversation).Project
        : WorkSidebarState.FindSelectedProject(_projects, _selectedWorkProjectId);

    private ProjectMountState? CurrentWorkspaceMount()
    {
        var project = CurrentWorkspaceProject();
        if (project is null) return null;
        Guid? selected = _workspaceMountSelections.TryGetValue(project.Id, out Guid id) ? id : project.DefaultMountId;
        return project.Mounts.FirstOrDefault(mount => mount.Id == selected) ?? project.Mounts.FirstOrDefault();
    }

    private string CurrentWorkspaceFilesBinding() => CurrentWorkspaceMount() is { } mount
        ? $"{mount.Id:D}/{mount.BindingRevision}/{mount.Path}" : CurrentWorkspaceDirectory() ?? string.Empty;

    private void SelectWorkspaceMount(Guid mountId)
    {
        if (_chatClosing || CurrentWorkspaceProject() is not { } project || !project.Mounts.Any(mount => mount.Id == mountId)) return;
        _workspaceMountSelections[project.Id] = mountId;
        bool wasBrowsing = _filesConversation == ActiveChatId;
        ApplyLayout();
        if (wasBrowsing && !WorkspaceHome.IsShowingAttachments) _ = BrowseWorkspaceFilesAsync();
    }

    private void CancelWorkspaceFileRead(bool forgetScope = false)
    {
        ++_filesGeneration;
        _workspaceFileRead?.Cancel(); _workspaceFileRead?.Dispose(); _workspaceFileRead = null;
        if (forgetScope) { _filesConversation = null; _filesMount = null; _filesBinding = null; _filesPath = null; }
    }

    private async Task BrowseWorkspaceFilesAsync(string? path = null)
    {
        if (_chatClosing || ActiveChatId is not Guid conversation) return;
        CancelWorkspaceFileRead();
        var cancellation = _workspaceFileRead = CancellationTokenSource.CreateLinkedTokenSource(_historyLifetime.Token);
        var cancellationToken = cancellation.Token;
        long generation = ++_filesGeneration;
        Guid? mountId = CurrentWorkspaceMount()?.Id;
        string binding = CurrentWorkspaceFilesBinding();
        if (path is null || _filesConversation != conversation || _filesMount != mountId || _filesBinding != binding) _filesRootDirectory = null;
        _filesConversation = conversation; _filesMount = mountId; _filesBinding = binding;
        WorkspaceHome.ShowFilesLoading();
        WorkspaceHome.DirectoryLoader = async (directory, token) =>
        {
            using var directoryApi = new WorkspaceFilesApiClient();
            return await directoryApi.ListAsync(conversation, mountId, directory, token);
        };
        try
        {
            using var api = new WorkspaceFilesApiClient();
            var listing = await api.ListAsync(conversation, mountId, path, cancellationToken);
            if (_chatClosing || generation != _filesGeneration || ActiveChatId != conversation || CurrentWorkspaceFilesBinding() != binding) return;
            _filesRootDirectory ??= CurrentWorkspaceDirectory() ?? listing.Path;
            _filesPath = listing.Path;
            WorkspaceHome.ShowFiles(listing, _filesRootDirectory);
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException or InvalidDataException or
            InvalidOperationException or System.Text.Json.JsonException)
        {
            if (!_chatClosing && !cancellationToken.IsCancellationRequested && generation == _filesGeneration && ActiveChatId == conversation)
                WorkspaceHome.ShowFilesError(error.Message);
        }
    }

    private async Task BrowseSavedAttachmentsAsync(int offset = 0)
    {
        if (_chatClosing || ActiveChatId is not Guid conversation) return;
        CancelWorkspaceFileRead();
        var cancellation = _workspaceFileRead = CancellationTokenSource.CreateLinkedTokenSource(_historyLifetime.Token);
        var token = cancellation.Token;
        long generation = ++_filesGeneration;
        Guid? project = WorkspaceHome.UsesProjectAttachments ? CurrentWorkspaceProject()?.Id : null;
        _filesConversation = conversation; _filesMount = CurrentWorkspaceMount()?.Id;
        _filesBinding = CurrentWorkspaceFilesBinding(); _attachmentOffset = Math.Max(0, offset);
        WorkspaceHome.ShowAttachmentsLoading();
        try
        {
            var page = project is null && FindChat(conversation).Chat?.IsPersisted != true
                ? new SavedAttachmentPage([])
                : await _attachmentsApiClient.ListSavedAsync(conversation, project, _attachmentOffset, token);
            if (_chatClosing || token.IsCancellationRequested || generation != _filesGeneration || ActiveChatId != conversation) return;
            WorkspaceHome.ShowAttachments(page);
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException or InvalidDataException or
            InvalidOperationException or System.Text.Json.JsonException)
        {
            if (!_chatClosing && !token.IsCancellationRequested && generation == _filesGeneration && ActiveChatId == conversation)
                WorkspaceHome.ShowFilesError(error.Message);
        }
    }

    private void OpenSavedWorkspaceAttachment(SavedAttachmentEntry entry)
    {
        if (_chatClosing || entry.ConversationId == Guid.Empty || entry.MessageId == Guid.Empty || entry.Id == Guid.Empty) return;
        var owner = FindChat(entry.ConversationId);
        if (owner.Chat is null || owner.Project?.Id != CurrentWorkspaceProject()?.Id) return;
        // Preserve the attachment's real chat identity; project aggregation never rebinds its preview to another chat.
        // 保留附件真实聊天身份；工作聚合不能把预览归属改绑到当前聊天。
        if (ActiveChatId != entry.ConversationId && owner.Project is { } project) SelectProjectChat(project, owner.Chat);
        if (ActiveChatId != entry.ConversationId) return;
        var resources = ResourcesFor(entry.ConversationId);
        string identity = $"{entry.MessageId:D}/{entry.Id:D}";
        if (resources.Attachments.Count >= 256 && !resources.Attachments.ContainsKey(identity))
            resources.Attachments.Remove(resources.Attachments.Keys.First());
        resources.Attachments[identity] = entry.ToAttachment();
        AddWorkspaceTab(entry.ConversationId, new($"attachment/{entry.ConversationId:D}/{identity}", "document", entry.Name,
            $"attachment:{identity}", entry.Sha256, UserSelected: true), select: true);
    }

    private ChatAttachment? FindWorkspaceAttachment(Guid conversation, Guid message, Guid attachment) =>
        ActiveMessages.FirstOrDefault(row => row.Message.Id == message)?.Message.Attachments.FirstOrDefault(item => item.Id == attachment)
        ?? ResourcesFor(conversation).Attachments.GetValueOrDefault($"{message:D}/{attachment:D}");

    private ConversationWorkTab LabelWorkspaceMount(Guid conversation, ConversationWorkTab tab)
    {
        var project = FindChat(conversation).Project;
        if (project is null || tab.Kind is not ("document" or "diagram") || string.IsNullOrWhiteSpace(tab.Target) ||
            !Path.IsPathFullyQualified(tab.Target)) return tab;
        string path = Path.GetFullPath(tab.Target);
        var candidates = tab.MountId is Guid selectedMount ? project.Mounts.Where(mount => mount.Id == selectedMount)
            : project.Mounts.OrderByDescending(mount => mount.Path.Length);
        var mount = candidates.FirstOrDefault(mount =>
        {
            string root = Path.TrimEndingDirectorySeparator(mount.Path);
            return string.Equals(path, root, StringComparison.OrdinalIgnoreCase) ||
                path.StartsWith(Path.EndsInDirectorySeparator(root) ? root : root + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase);
        });
        if (mount is null) return tab;
        return tab with { MountId = mount.Id, Title = project.Mounts.Count > 1 ? $"{Path.GetFileName(path)} · {mount.Name}" : tab.Title };
    }
}
