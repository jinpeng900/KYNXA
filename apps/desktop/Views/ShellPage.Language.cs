using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private bool _languageUpdatesAttached;

    private void AttachLanguageUpdates()
    {
        if (_languageUpdatesAttached) return;
        UiText.LanguageChanged += InterfaceLanguageChanged;
        _languageUpdatesAttached = true;
    }

    private void DetachLanguageUpdates()
    {
        if (!_languageUpdatesAttached) return;
        UiText.LanguageChanged -= InterfaceLanguageChanged;
        _languageUpdatesAttached = false;
    }

    private void InterfaceLanguageChanged(object? sender, EventArgs args)
    {
        if (!DispatcherQueue.HasThreadAccess)
        {
            DispatcherQueue.TryEnqueue(() => InterfaceLanguageChanged(sender, args));
            return;
        }
        if (_projectViewClosed || !IsLoaded) return;
        _layout.InterfaceLanguage = UiText.Language;
        bool chat = ViewModel.IsChatMode;
        AutomationProperties.SetHelpText(WorkModeButton, UiText.Get(chat ? "未选中" : "已选中"));
        AutomationProperties.SetHelpText(ChatModeButton, UiText.Get(chat ? "已选中" : "未选中"));
        PromptTextBox.PlaceholderText = UiText.Get(chat ? "向 KYNXA 提问任何问题..." : "描述你想完成的工作...");
        UpdateModelPickerLabel();
        _historySearchFlyout?.Hide();
        ClearActionFeedback();
        UpdatePermissionPickerLabel();
        UpdateSendButtonState();
        UpdateComposerExpandVisual();
        UpdateWorkspacePickerVisibility();
        UpdateWorkTaskLanguage();
        AutomationProperties.SetName(WorkRecentToggle,
            UiText.Get(_layout.WorkRecentExpanded ? "收起最近的工作聊天" : "展开最近的工作聊天"));
        foreach (var row in ProjectEntries) row.RefreshLanguage();
        foreach (var row in WorkRecentEntries) row.RefreshLanguage();
        foreach (var row in WorkTaskEntries) row.RefreshLanguage();
        ApplyLayout();
        // Keep the current draft, active conversation, scroll position and reply stream intact.
        // 切换语言时保留当前草稿、活动聊天、滚动位置与回复流。
    }
}
