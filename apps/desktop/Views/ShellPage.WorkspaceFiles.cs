using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private readonly Dictionary<Guid, Guid> _workspaceMountSelections = [];
    private Guid? _filesConversation;
    private Guid? _filesMount;
    private long _filesGeneration;
    private string? _filesRootDirectory;
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

    private void SelectWorkspaceMount(Guid mountId)
    {
        if (_chatClosing || CurrentWorkspaceProject() is not { } project || !project.Mounts.Any(mount => mount.Id == mountId)) return;
        _workspaceMountSelections[project.Id] = mountId;
        bool wasBrowsing = _filesConversation == ActiveChatId;
        ApplyLayout();
        if (wasBrowsing) _ = BrowseWorkspaceFilesAsync();
    }

    private async Task BrowseWorkspaceFilesAsync(string? path = null)
    {
        if (_chatClosing || ActiveChatId is not Guid conversation) return;
        long generation = ++_filesGeneration;
        Guid? mountId = CurrentWorkspaceMount()?.Id;
        if (path is null || _filesConversation != conversation || _filesMount != mountId) _filesRootDirectory = null;
        _filesConversation = conversation; _filesMount = mountId;
        WorkspaceHome.ShowFilesLoading();
        try
        {
            using var api = new WorkspaceFilesApiClient();
            var listing = await api.ListAsync(conversation, mountId, path, _historyLifetime.Token);
            if (_chatClosing || generation != _filesGeneration || ActiveChatId != conversation || CurrentWorkspaceMount()?.Id != mountId) return;
            _filesRootDirectory ??= CurrentWorkspaceDirectory() ?? listing.Path;
            WorkspaceHome.ShowFiles(listing, _filesRootDirectory);
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException or InvalidDataException or
            InvalidOperationException or System.Text.Json.JsonException)
        {
            if (!_chatClosing && generation == _filesGeneration && ActiveChatId == conversation)
                WorkspaceHome.ShowFilesError(error.Message);
        }
    }

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
