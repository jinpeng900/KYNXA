using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private MemoryManagementWindow? _memoryManagementWindow;

    private void OpenMemoryManagement()
    {
        if (!_projectsReady || StoragePaths.IsMigrating) return;
        if (_memoryManagementWindow is { } existing)
        {
            if (existing.AppWindow.Presenter is OverlappedPresenter presenter) presenter.Restore();
            existing.Activate();
            return;
        }
        var targets = new List<MemoryTarget>();
        foreach (var project in _projects)
        {
            if (!project.IsFolderlessWorkspace) targets.Add(new(MemoryScopes.Project, project.Id, project.Name, project.IsArchived));
            foreach (var chat in project.Chats.Where(chat => chat.CanPersist && !chat.IsSample))
                targets.Add(new(MemoryScopes.Chat, chat.Id, $"{project.Name} / {chat.Title}"));
        }
        foreach (var chat in _standaloneChats.Where(chat => chat.CanPersist && !chat.IsSample))
            targets.Add(new(MemoryScopes.Chat, chat.Id, chat.Title));
        var active = ViewModel.IsChatMode ? _activeStandaloneChat : _activeProjectChat;
        var initial = active is { CanPersist: true } ? targets.FirstOrDefault(target => target.Scope == MemoryScopes.Chat && target.Id == active.Id)
            : targets.FirstOrDefault(target => target.Scope == MemoryScopes.Project && target.Id == _selectedWorkProjectId);
        var window = new MemoryManagementWindow(targets, initial);
        _memoryManagementWindow = window;
        var owner = App.Window.AppWindow;
        window.AppWindow.Move(new Windows.Graphics.PointInt32(owner.Position.X + Math.Max(0, (owner.Size.Width - 960) / 2),
            owner.Position.Y + Math.Max(0, (owner.Size.Height - 680) / 2)));
        void CloseMemory(object sender, WindowEventArgs e) => window.CloseForOwner();
        App.Window.Closed += CloseMemory;
        window.Closed += (_, _) =>
        {
            App.Window.Closed -= CloseMemory;
            _memoryManagementWindow = null;
        };
        window.Activate();
    }
}
