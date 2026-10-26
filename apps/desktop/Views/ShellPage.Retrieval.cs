using KYNXA_Desktop.Services;
using KYNXA_Desktop.Models.UI;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private RetrievalSettingsWindow? _retrievalSettingsWindow;
    private ProjectState? _settingsRetrievalProject;

    private void OpenRetrievalSettings(ProjectState? project = null)
    {
        if (!_projectsReady || StoragePaths.IsMigrating || (project is not null && !IsAvailableProject(project))) return;
        if (_retrievalSettingsWindow is { } existing && existing.ProjectId != project?.Id)
        {
            if (existing.HasPendingChanges)
            {
                NavigateSettings("retrieval");
                return;
            }
            existing.DisposeEmbedded();
            _retrievalSettingsWindow = null;
            _settingsWorkspace?.ResetPage("retrieval");
        }
        _settingsRetrievalProject = project;
        NavigateSettings("retrieval");
    }

    private FrameworkElement BuildSettingsRetrievalPage(Window owner)
    {
        var project = _settingsRetrievalProject;
        var controller = new RetrievalSettingsWindow(project?.Id, project?.Name,
            ProjectMountPresentation.WorkspaceMount(project)?.Path ?? project?.FolderPath, settingsOwner: owner);
        _retrievalSettingsWindow = controller;
        return controller.AttachToSettings(owner);
    }
}
