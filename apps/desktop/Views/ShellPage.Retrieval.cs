using KYNXA_Desktop.Services;
using KYNXA_Desktop.Models.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private RetrievalSettingsWindow? _retrievalSettingsWindow;

    private void OpenRetrievalSettings(ProjectState? project = null)
    {
        if (!_projectsReady || StoragePaths.IsMigrating || (project is not null && !IsAvailableProject(project))) return;
        if (_retrievalSettingsWindow is { } existing)
        {
            if (existing.ProjectId == project?.Id || existing.HasPendingChanges)
            {
                if (existing.AppWindow.Presenter is OverlappedPresenter presenter) presenter.Restore();
                existing.Activate();
                return;
            }
            existing.CloseForOwner();
        }
        var window = new RetrievalSettingsWindow(project?.Id, project?.Name, project?.FolderPath);
        _retrievalSettingsWindow = window;
        var owner = App.Window.AppWindow;
        window.AppWindow.Move(new Windows.Graphics.PointInt32(owner.Position.X + Math.Max(0, (owner.Size.Width - 720) / 2),
            owner.Position.Y + Math.Max(0, (owner.Size.Height - 760) / 2)));
        void CloseRetrieval(object sender, WindowEventArgs args) => window.CloseForOwner();
        App.Window.Closed += CloseRetrieval;
        window.Closed += (_, _) =>
        {
            App.Window.Closed -= CloseRetrieval;
            if (_retrievalSettingsWindow == window) _retrievalSettingsWindow = null;
        };
        window.Activate();
    }
}
