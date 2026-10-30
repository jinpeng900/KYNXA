using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using Windows.Storage;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private sealed class ProjectCreationDraft
    {
        public Guid ProjectId { get; } = Guid.NewGuid();
        public TaskCompletionSource<ProjectState?> Completion { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
    }
    private ProjectCreationDraft? _projectCreation;
    private Guid? _folderEditorChat, _folderEditorProject;
    private bool _folderEditorChatMode;

    private Task<ProjectState?> ShowProjectCreationAsync(StorageFolder? initialFolder = null, bool reuseFolderProject = false)
    {
        if (_projectCreation is { } existing) return existing.Completion.Task;
        if (_projectViewClosed || _chatClosing || _savingOnClose || XamlRoot is null)
            return Task.FromResult<ProjectState?>(null);
        var draft = _projectCreation = new ProjectCreationDraft();
        Guid? originalChat = ActiveChatId, originalProject = _selectedWorkProjectId;
        bool originalMode = ViewModel.IsChatMode;
        bool Current() => !_projectViewClosed && !_chatClosing && !_savingOnClose && ReferenceEquals(_projectCreation, draft) &&
            ActiveChatId == originalChat && _selectedWorkProjectId == originalProject && ViewModel.IsChatMode == originalMode;
        var editor = new WorkspaceFolderEditor(UiText.Get("创建项目"), initialFolder?.DisplayName ?? string.Empty,
            initialFolder?.Path, creating: true, async input =>
            {
                if (!Current()) throw new OperationCanceledException();
                ProjectState? created = null;
                await CommitFolderEditAsync(async () =>
                {
                    if (!Current()) throw new OperationCanceledException();
                    var existingProject = reuseFolderProject && input.FolderPath is { } requestedPath
                        ? _projects.FirstOrDefault(candidate => !candidate.IsFolderlessWorkspace &&
                            (ProjectMountPresentation.WorkspaceMount(candidate)?.Path ?? candidate.FolderPath) is { } path &&
                            string.Equals(Path.TrimEndingDirectorySeparator(path), requestedPath, StringComparison.OrdinalIgnoreCase))
                        : null;
                    if (existingProject is not null)
                    {
                        bool archived = existingProject.IsArchived;
                        existingProject.IsArchived = false;
                        try { await SaveProjectsAndRenderAsync(); }
                        catch { existingProject.IsArchived = archived; throw; }
                        created = existingProject;
                        return;
                    }
                    ProjectMountState? folder = input.FolderPath is { } folderPath
                        ? new(Guid.NewGuid(), FolderDisplayName(folderPath), folderPath) : null;
                    var project = new ProjectState { Id = draft.ProjectId, Name = input.Name,
                        Mounts = folder is null ? [] : [folder], DefaultMountId = folder?.Id, FolderPath = folder?.Path };
                    _projects.Insert(0, project);
                    try { await SaveProjectsAndRenderAsync(); created = project; }
                    catch { _projects.Remove(project); RenderProjects(); throw; }
                });
                bool current = Current();
                if (ReferenceEquals(_projectCreation, draft)) _projectCreation = null;
                draft.Completion.TrySetResult(current ? created : null);
            });
        editor.Closed += (_, accepted) =>
        {
            if (ReferenceEquals(_projectCreation, draft)) _projectCreation = null;
            if (!accepted) draft.Completion.TrySetResult(null);
        };
        ShowInlineFolderEditor(editor);
        return draft.Completion.Task;
    }

    private void ShowInlineFolderEditor(WorkspaceFolderEditor editor)
    {
        WorkspaceHome.CancelFolderEditor();
        bool previousPreview = _layout.PreviewVisible, previousEmpty = _emptyWorkPanelRequested;
        _folderEditorChat = ActiveChatId; _folderEditorProject = _selectedWorkProjectId;
        _folderEditorChatMode = ViewModel.IsChatMode;
        WorkspaceHome.ShowFolderEditor(editor);
        editor.Closed += (_, _) =>
        {
            _layout.PreviewVisible = previousPreview;
            _emptyWorkPanelRequested = previousEmpty;
            if (!_projectViewClosed && !_chatClosing) DispatcherQueue.TryEnqueue(ApplyLayout);
        };
        _layout.PreviewVisible = true;
        ApplyLayout();
        editor.FocusInput();
    }

    private void CancelFolderEditorForChangedScope()
    {
        if (WorkspaceHome.IsFolderEditorVisible && (_folderEditorChat != ActiveChatId ||
            _folderEditorProject != _selectedWorkProjectId || _folderEditorChatMode != ViewModel.IsChatMode))
            WorkspaceHome.CancelFolderEditor();
    }

    private async Task CommitFolderEditAsync(Func<Task> commit)
    {
        if (!_projectsReady || _projectActionPending || _savingOnClose || _chatClosing || _projectViewClosed)
            throw new InvalidOperationException(UiText.Get("请稍后重试。"));
        _projectActionPending = true;
        UpdateProjectNoticeAction();
        UpdateSendButtonState();
        try { await commit(); }
        finally
        {
            _projectActionPending = false;
            // Folder commits can open an undo notice while pending; restore its action and dismiss clock together.
            // 文件夹提交可能在忙碌时打开撤销提示，结束后须一并恢复操作按钮和消失计时。
            UpdateProjectNoticeAction();
            if (!_savingOnClose && !_projectViewClosed) UpdateSendButtonState();
        }
    }

    private static string FolderDisplayName(string path)
    { string name = Path.GetFileName(Path.TrimEndingDirectorySeparator(path)); return name.Length == 0 ? path : name; }
}
