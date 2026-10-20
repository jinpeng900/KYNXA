using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private MemoryManagementWindow? _memoryManagementWindow;
    private IReadOnlyList<MemoryTarget>? _settingsMemoryTargets;

    private void OpenMemoryManagement()
    {
        if (!_projectsReady || StoragePaths.IsMigrating) return;
        NavigateSettings("memory");
    }

    private List<MemoryTarget> CurrentSettingsMemoryTargets()
    {
        var targets = new List<MemoryTarget>();
        foreach (var project in _projects)
        {
            if (!project.IsFolderlessWorkspace) targets.Add(new(MemoryScopes.Project, project.Id, project.Name, project.IsArchived));
            foreach (var chat in project.Chats.Where(chat => chat.CanPersist && !chat.IsSample))
                targets.Add(new(MemoryScopes.Chat, chat.Id, $"{project.Name} / {chat.Title}"));
        }
        foreach (var chat in _standaloneChats.Where(chat => chat.CanPersist && !chat.IsSample))
            targets.Add(new(MemoryScopes.Chat, chat.Id, chat.Title));
        return targets;
    }

    private bool RefreshSettingsMemoryContext()
    {
        if (_memoryManagementWindow is not { } current ||
            (_settingsMemoryTargets is { } targets && targets.SequenceEqual(CurrentSettingsMemoryTargets()))) return true;
        // Keep unfinished edits bound to their original target; never silently redirect a save.
        // 未完成编辑仍绑定原目标，不在用户不知情时改变保存范围。
        if (current.HasPendingChanges) return false;
        current.DisposeEmbedded();
        _memoryManagementWindow = null;
        _settingsWorkspace?.ResetPage("memory");
        _settingsWorkspace?.SelectPage("memory");
        return true;
    }

    private FrameworkElement BuildSettingsMemoryPage(Window owner)
    {
        var targets = CurrentSettingsMemoryTargets();
        _settingsMemoryTargets = targets;
        var active = ViewModel.IsChatMode ? _activeStandaloneChat : _activeProjectChat;
        var initial = active is { CanPersist: true } ? targets.FirstOrDefault(target => target.Scope == MemoryScopes.Chat && target.Id == active.Id)
            : targets.FirstOrDefault(target => target.Scope == MemoryScopes.Project && target.Id == _selectedWorkProjectId);
        var controller = new MemoryManagementWindow(targets, initial, settingsOwner: owner);
        _memoryManagementWindow = controller;
        return controller.AttachToSettings(owner);
    }
}
