using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private readonly AgentApiClient _agentApiClient = new();
    private ToolManagementWindow? _toolManagementWindow;
    private bool _settingsToolsSkills;
    private Guid? _settingsToolsConversationId;

    private Guid? CurrentSettingsToolsConversationId()
    {
        var active = ViewModel.IsChatMode ? _activeStandaloneChat : _activeProjectChat;
        return active is { CanPersist: true } ? active.Id : null;
    }

    private bool RefreshSettingsToolsContext()
    {
        if (_toolManagementWindow is not { } current || _settingsToolsConversationId == CurrentSettingsToolsConversationId()) return true;
        // Reuse cached views only within the same conversation; dirty forms retain their original scope.
        // 仅在同一会话中复用缓存页面；含未保存编辑时保留原范围。
        if (current.HasPendingChanges) return false;
        current.DisposeEmbedded();
        _toolManagementWindow = null;
        _settingsWorkspace?.ResetPage("tools");
        _settingsWorkspace?.SelectPage("tools");
        return true;
    }

    private void OpenAgentTools(bool skills = false)
    {
        if (!_projectsReady || StoragePaths.IsMigrating || _projectActionPending || _sendingPrompt)
        {
            ShowActionFeedback(UiText.Get("正在准备或保存数据，请稍后打开工具与技能。"));
            return;
        }
        _settingsToolsSkills = skills;
        NavigateSettings("tools");
        _toolManagementWindow?.ShowSection(skills);
    }

    private FrameworkElement BuildSettingsToolsPage(Window owner)
    {
        _settingsToolsConversationId = CurrentSettingsToolsConversationId();
        var controller = new ToolManagementWindow(conversationId: _settingsToolsConversationId, settingsOwner: owner);
        _toolManagementWindow = controller;
        var view = controller.AttachToSettings(owner);
        controller.ShowSection(_settingsToolsSkills);
        return view;
    }
}
