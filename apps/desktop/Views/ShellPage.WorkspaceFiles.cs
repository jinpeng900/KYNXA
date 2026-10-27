using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA.Contracts;
using Windows.Storage.Pickers;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private Guid? _filesConversation;
    private Guid? _filesMount;
    private string? _filesBinding;
    private long _filesGeneration;
    private string? _filesRootDirectory;
    private string? _filesPath;
    private int _attachmentOffset;
    private CancellationTokenSource? _workspaceFileRead;
    private string? _retrievalActivationKey;
    private bool _pickingLocalWorkspaceFile;

    private async Task PickLocalWorkspaceFileAsync()
    {
        if (_pickingLocalWorkspaceFile || _chatClosing || !IsLoaded || HasPresentationModal()
            || ActiveChatId is not Guid conversation) return;
        _pickingLocalWorkspaceFile = true;
        try
        {
            var picker = new FileOpenPicker { SuggestedStartLocation = PickerLocationId.DocumentsLibrary };
            picker.FileTypeFilter.Add("*");
            WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(App.Window));
            var file = await picker.PickSingleFileAsync();
            if (file is null || _chatClosing || !IsLoaded || ActiveChatId != conversation) return;
            if (!Path.IsPathFullyQualified(file.Path))
            {
                ShowActionFeedback(UiText.Get("所选文件没有可用的本机路径。"));
                return;
            }
            var (chat, project) = FindChat(conversation);
            if (chat is null) return;
            chat.KeepWorkspace = true;
            if (!chat.IsPersisted) await SaveChatChangesAsync(project);
            if (_chatClosing || !IsLoaded || ActiveChatId != conversation) return;
            // The picker grants this file's preview, never another folder or model-wide access.
            // 选择器只授权预览此文件，不增加工作文件夹或模型的目录权限。
            WorkspaceHome.CancelFolderEditor();
            AddWorkspaceTab(conversation, ConversationWorkspaceSources.File(file.Path, file.Name, userSelected: true) with
                { PresentationPriority = WorkspacePresentationPriority.Requested }, select: true);
        }
        catch (Exception error) when (error is System.Runtime.InteropServices.COMException or IOException or HttpRequestException
            or UnauthorizedAccessException or InvalidOperationException or ArgumentException)
        {
            if (!_chatClosing && ActiveChatId == conversation)
                ShowActionFeedback(UiText.Get("本机文件打开失败，请重试。"));
        }
        finally { _pickingLocalWorkspaceFile = false; }
    }

    private void NotifyActiveRetrievalProject(ProjectState? project)
    {
        if (!_projectsReady || _chatClosing || _projectViewClosed) return;
        var mount = ProjectMountPresentation.WorkspaceMount(project);
        string key = project is null ? "global" : $"{project.Id:D}/{mount?.Id:D}/{mount?.BindingRevision}/{mount?.Path ?? project.FolderPath}";
        if (_retrievalActivationKey == key) return;
        _retrievalActivationKey = key;
        _ = ActivateRetrievalProjectAsync(mount is not null ? project!.Id : null);
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

    private ProjectMountState? CurrentWorkspaceMount() => ProjectMountPresentation.WorkspaceMount(CurrentWorkspaceProject());

    private string CurrentWorkspaceFilesBinding() => CurrentWorkspaceMount() is { } mount
        ? $"{mount.Id:D}/{mount.BindingRevision}/{mount.Path}" : CurrentWorkspaceDirectory() ?? string.Empty;

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

    private ConversationWorkTab BindWorkspaceFile(Guid conversation, ConversationWorkTab tab)
    {
        if (tab.MountId is not null || tab.Kind is not ("document" or "diagram") || string.IsNullOrWhiteSpace(tab.Target) ||
            !Path.IsPathFullyQualified(tab.Target)) return tab;
        var mount = ProjectMountPresentation.WorkspaceMount(FindChat(conversation).Project);
        if (mount is null) return tab;
        string path = Path.GetFullPath(tab.Target);
        string root = Path.TrimEndingDirectorySeparator(mount.Path);
        return string.Equals(path, root, StringComparison.OrdinalIgnoreCase) ||
            path.StartsWith(Path.EndsInDirectorySeparator(root) ? root : root + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase)
            ? tab with { MountId = mount.Id } : tab;
    }
}
