using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private bool _projectRenderQueued;
    private readonly HashSet<Guid> _pendingProjectExpansions = [];
    private Guid? _projectToReveal;
    private int _projectRenderVersion;

    private void RenderProjects(Guid? expandProject = null)
    {
        if (_projectViewClosed) return;
        if (expandProject is Guid id) _pendingProjectExpansions.Add(id);
        if (_projectRenderQueued) return;

        // ItemInvoked and pointer handlers must finish before changing their tree's
        // nodes. Merge requests and read the latest state when the callback runs.
        _projectRenderQueued = DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, ApplyProjectRows);
    }

    private void ApplyProjectRows()
    {
        _projectRenderQueued = false;
        if (_projectViewClosed || !IsLoaded) return;
        int version = ++_projectRenderVersion;
        _renderingProjects = true;
        try
        {
            ProjectTreeReconciler.Update(ProjectEntries, _projects, _activeProjectChat?.Id,
                _collapsedByUser, _pendingProjectExpansions, _selectedWorkProjectId);
            foreach (var child in ProjectEntries.SelectMany(project => project.Children))
                child.IsReplying = _pendingReplies.ContainsKey(child.Chat!.Id);
            _pendingProjectExpansions.Clear();
        }
        finally { _renderingProjects = false; }
        RebuildWorkTasks();

        if (_projectToReveal is not Guid id) return;
        _projectToReveal = null;
        DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () =>
        {
            if (_projectViewClosed || !IsLoaded || !ProjectTree.IsLoaded
                || version != _projectRenderVersion) return;
            var entry = ProjectEntries.FirstOrDefault(project => project.Project.Id == id);
            if (entry is not null && ProjectTree.ContainerFromItem(entry) is FrameworkElement { IsLoaded: true } row)
                row.StartBringIntoView();
        });
    }
}
