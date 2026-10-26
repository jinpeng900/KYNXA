using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private Func<Task>? _undoSidebarChange;
    private bool _projectNoticeUndoPending;

    private sealed record ProjectFolderBinding(string? FolderPath, ProjectMountState[] Mounts, Guid? DefaultMountId);

    private void ShowProjectNotice(string message, Func<Task>? undo = null)
    {
        // A notice owns exactly its own action; a later message must never reuse a previous undo.
        // 每条提示只持有自己的操作，后来的消息不得复用上一条撤销。
        _undoSidebarChange = undo;
        ProjectNotice.Message = message;
        ProjectNotice.IsOpen = true;
        ProjectNotice.RestartAutoDismiss();
        UpdateProjectNoticeAction();
    }

    private void UpdateProjectNoticeAction()
    {
        ProjectNotice.IsAutoDismissPaused = _projectNoticeUndoPending || _projectActionPending;
        ProjectNoticeUndoButton.Visibility = _undoSidebarChange is null ? Visibility.Collapsed : Visibility.Visible;
        ProjectNoticeUndoButton.IsEnabled = _undoSidebarChange is not null && ProjectNotice.IsOpen &&
            !_projectNoticeUndoPending && !_projectActionPending && _projectsReady && !_savingOnClose && !_projectViewClosed && !_chatClosing;
    }

    private void ProjectNotice_Closed(InfoBar sender, InfoBarClosedEventArgs args)
    {
        if (sender.IsOpen) return;
        _undoSidebarChange = null;
        UpdateProjectNoticeAction();
    }

    private void PreserveUndoConversationProjections(ConversationCatalog catalog)
    {
        CaptureProjectDraft();
        CaptureStandaloneDraft();
        var previousWorkChat = _activeProjectChat;
        var previousStandaloneChat = _activeStandaloneChat;
        var previousProject = _projects.FirstOrDefault(project => project.Chats.Contains(previousWorkChat!));
        var currentProject = catalog.Projects.FirstOrDefault(project => project.Id == previousProject?.Id && !project.IsArchived);

        ProjectChatState? Retain(ProjectChatState? previous, List<ProjectChatState>? current)
        {
            if (previous is null || current is null) return null;
            int index = current.FindIndex(chat => chat.Id == previous.Id);
            if (index < 0)
            {
                // Only a never-saved local draft may be absent from the authoritative catalog.
                // 只有从未保存的本地草稿可以不在正式目录中，已删除或移出范围的聊天不能复活。
                if (previous.IsPersisted || previous.IsArchived || catalog.Chats.Concat(catalog.Projects.SelectMany(project => project.Chats))
                    .Any(chat => chat.Id == previous.Id)) return null;
                current.Insert(0, previous);
                return previous;
            }
            var metadata = current[index];
            if (metadata.IsArchived) return null;
            // Keep body/draft and history identity; the refreshed catalog still owns all formal metadata.
            // 保留正文、草稿及历史投影身份，正式元数据仍以重新读取的目录为准。
            previous.Title = metadata.Title;
            previous.IsSample = metadata.IsSample;
            previous.IsPinned = metadata.IsPinned;
            previous.IsArchived = metadata.IsArchived;
            previous.IsPersisted = metadata.IsPersisted;
            current[index] = previous;
            return previous;
        }

        _activeProjectChat = Retain(previousWorkChat, currentProject?.Chats);
        _activeStandaloneChat = Retain(previousStandaloneChat, catalog.Chats);
        if (previousWorkChat is not null)
        {
            _workDraft = _activeProjectChat?.Draft ?? string.Empty;
            _workConversationTitle = _activeProjectChat is { } workChat ? WorkChatTitle(currentProject!, workChat) : string.Empty;
        }
        if (previousStandaloneChat is not null)
        {
            _chatDraft = _activeStandaloneChat?.Draft ?? string.Empty;
            _chatConversationTitle = _activeStandaloneChat?.Title ?? string.Empty;
        }
        if (ViewModel.IsChatMode ? previousStandaloneChat is not null && _activeStandaloneChat is null
            : previousWorkChat is not null && _activeProjectChat is null)
            PromptTextBox.Text = ViewModel.Prompt = string.Empty;
    }

    private async void UndoSidebarChange_Click(object sender, RoutedEventArgs args)
    {
        if (_projectNoticeUndoPending || _undoSidebarChange is not { } undo) return;
        _projectNoticeUndoPending = true;
        UpdateProjectNoticeAction();
        try
        {
            await RunProjectActionAsync(async () =>
            {
                if (!ReferenceEquals(_undoSidebarChange, undo)) return;
                await undo();
                if (!ReferenceEquals(_undoSidebarChange, undo)) return;
                _undoSidebarChange = null;
                ProjectNotice.IsOpen = false;
            });
        }
        finally
        {
            _projectNoticeUndoPending = false;
            UpdateProjectNoticeAction();
        }
    }

    private InvalidOperationException ProjectUndoUnavailable()
    {
        _undoSidebarChange = null;
        UpdateProjectNoticeAction();
        return new InvalidOperationException(UiText.Get("目标已变化，请重新读取。"));
    }

    private ProjectState RequireUndoProject(Guid projectId) =>
        _projects.FirstOrDefault(project => project.Id == projectId && IsAvailableProject(project)) ?? throw ProjectUndoUnavailable();

    private ProjectState? UndoChatProject(Guid? projectId) => projectId is Guid id
        ? _projects.FirstOrDefault(project => project.Id == id && !project.IsArchived) ?? throw ProjectUndoUnavailable()
        : null;

    private void RefreshUndoChatRows(ProjectState? project)
    {
        if (project is null) RebuildStandaloneRows();
        else RenderProjects();
    }

    private static ProjectFolderBinding CaptureProjectFolderBinding(ProjectState project)
    {
        var mount = ProjectMountPresentation.WorkspaceMount(project);
        return new(mount?.Path ?? project.FolderPath, mount is null ? [] : [mount], mount?.Id);
    }

    private static bool SameFolderPath(string? first, string? second) => string.Equals(
        first is null ? null : Path.TrimEndingDirectorySeparator(first),
        second is null ? null : Path.TrimEndingDirectorySeparator(second), StringComparison.OrdinalIgnoreCase);

    private static bool MatchesProjectFolderBinding(ProjectState project, ProjectFolderBinding binding) =>
        CaptureProjectFolderBinding(project) is { } current &&
        current.DefaultMountId == binding.DefaultMountId && SameFolderPath(current.FolderPath, binding.FolderPath) &&
        current.Mounts.Length == binding.Mounts.Length && current.Mounts.Zip(binding.Mounts).All(pair =>
            pair.First.Id == pair.Second.Id && pair.First.Name == pair.Second.Name &&
            pair.First.BindingRevision == pair.Second.BindingRevision && SameFolderPath(pair.First.Path, pair.Second.Path));

    private static void ApplyProjectFolderBinding(ProjectState project, ProjectFolderBinding binding)
    {
        var mount = ProjectMountPresentation.WorkspaceMount(new ProjectState
        { FolderPath = binding.FolderPath, Mounts = binding.Mounts.ToList(), DefaultMountId = binding.DefaultMountId });
        project.FolderPath = mount?.Path;
        project.Mounts = mount is null ? [] : [mount];
        project.DefaultMountId = mount?.Id;
    }

    private void ShowProjectFolderNotice(ProjectState project, ProjectFolderBinding previous, string message)
    {
        Guid projectId = project.Id;
        var expected = CaptureProjectFolderBinding(project);
        ShowProjectNotice(message, async () =>
        {
            var current = RequireUndoProject(projectId);
            if (!MatchesProjectFolderBinding(current, expected)) throw ProjectUndoUnavailable();
            var rollback = CaptureProjectFolderBinding(current);
            // Restoring a removed or rebound root gets a fresh version; prior task capabilities must stay revoked.
            // 恢复已移除或改绑的根目录使用新版本，不能重新激活旧任务的目录能力。
            var restored = previous.Mounts.Select(mount =>
            {
                var existing = current.Mounts.FirstOrDefault(candidate => candidate.Id == mount.Id);
                ulong revision = Math.Max(mount.BindingRevision, existing?.BindingRevision ?? 0);
                if (existing is null || !SameFolderPath(existing.Path, mount.Path)) revision = checked(revision + 1);
                return mount with { BindingRevision = revision };
            }).ToArray();
            ApplyProjectFolderBinding(current, previous with { Mounts = restored });
            try { await SaveProjectsAndRenderAsync(); }
            catch
            {
                ApplyProjectFolderBinding(current, rollback);
                RenderProjects();
                UpdateMountedWorkspacePresentation();
                throw;
            }
        });
    }
}
