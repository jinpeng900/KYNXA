using KYNXA_Desktop.Services;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private void StandaloneChatMore_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: RecentConversation item } button) return;
        var chat = _standaloneChats.FirstOrDefault(candidateChat => candidateChat.Id == item.Id);
        if (chat is null) return;
        ShowSidebarMenu(button, CreateChatMenu(chat));
    }

    private MenuFlyout CreateChatMenu(ProjectChatState chat, ProjectState? project = null)
    {
        var menu = new MenuFlyout
        {
            Placement = FlyoutPlacementMode.BottomEdgeAlignedLeft,
            MenuFlyoutPresenterStyle = (Style)Application.Current.Resources["KynxaProjectMenuPresenterStyle"]
        };
        void Item(string title, string glyph, Func<Task> action)
        {
            var item = new MenuFlyoutItem
            {
                Text = title, Icon = new FontIcon { Glyph = glyph },
                Style = (Style)Application.Current.Resources["KynxaWorkMenuItemStyle"]
            };
            item.Click += async (_, _) => await RunProjectActionAsync(action);
            menu.Items.Add(item);
        }

        Item(chat.IsPinned ? UiText.Get("取消置顶") : UiText.Get("置顶"), "\uE718", async () =>
        {
            chat.IsPinned = !chat.IsPinned;
            await SaveChatChangesAsync(project);
        });
        Item(UiText.Get("删除"), "\uE74D", async () =>
        {
            CaptureProjectDraft();
            CaptureStandaloneDraft();
            var chats = project?.Chats ?? _standaloneChats;
            int index = chats.IndexOf(chat);
            if (index < 0) return;
            ClearActiveChat(chat);
            chats.RemoveAt(index);
            CancelPendingReply(chat.Id);
            await SaveChatChangesAsync(project);
            await MessageTimePresentation.RestoreAsync(chat.Id, chat.Messages.ToArray());
            if (!await MessageTimePresentation.RemoveConversationAsync(chat.Id))
                ShowActionFeedback(UiText.Get("回复时间未能保存到本机缓存，聊天内容不受影响。"));
            _undoSidebarChange = async () =>
            {
                chats.Insert(Math.Min(index, chats.Count), chat);
                await SaveChatChangesAsync(project);
                await RestoreDeletedChatTimesAsync(chat);
            };
            ShowChatNotice(string.Format(UiText.Get("已删除“{0}”"), chat.Title));
        });
        Item(UiText.Get("重命名"), "\uE70F", async () =>
        {
            string? name = await AskChatNameAsync(chat.Title);
            if (name is null) return;
            chat.Title = name;
            if (_activeStandaloneChat == chat) _chatConversationTitle = name;
            if (_activeProjectChat == chat) _workConversationTitle = WorkChatTitle(project!, chat);
            await SaveChatChangesAsync(project);
            UpdateConversationTitle();
        });
        Item(UiText.Get("归档"), "\uE7B8", async () =>
        {
            CaptureProjectDraft();
            CaptureStandaloneDraft();
            ClearActiveChat(chat);
            chat.IsArchived = true;
            await SaveChatChangesAsync(project);
            _undoSidebarChange = async () =>
            {
                chat.IsArchived = false;
                await SaveChatChangesAsync(project);
            };
            ShowChatNotice(string.Format(UiText.Get("已归档“{0}”"), chat.Title));
        });
        return menu;
    }

    private async Task SaveChatChangesAsync(ProjectState? project)
    {
        CaptureProjectDraft();
        CaptureStandaloneDraft();
        if (project is null)
        {
            await _projectStore.SaveChatsAsync(_standaloneChats);
            RebuildStandaloneRows();
        }
        else
        {
            await _projectStore.SaveAsync(_projects);
            RenderProjects();
        }
    }

    private void ClearActiveChat(ProjectChatState chat)
    {
        bool clearComposer = false;
        if (_activeProjectChat == chat)
        {
            _activeProjectChat = null;
            _workDraft = string.Empty;
            _workConversationTitle = KYNXA_Desktop.Services.WorkSidebarState.FindSelectedProject(_projects, _selectedWorkProjectId)?.Name
                ?? string.Empty;
            clearComposer = !ViewModel.IsChatMode;
        }
        if (_activeStandaloneChat == chat)
        {
            _activeStandaloneChat = null;
            _chatDraft = _chatConversationTitle = string.Empty;
            clearComposer = ViewModel.IsChatMode;
        }
        if (clearComposer) PromptTextBox.Text = ViewModel.Prompt = string.Empty;
        UpdateConversationTitle();
        UpdateConversationPresentation();
    }

    private void ShowChatNotice(string message)
    {
        ProjectNotice.Message = message;
        ProjectNotice.IsOpen = true;
    }

    private async Task<string?> AskChatNameAsync(string value)
    {
        var input = new TextBox
        {
            FontFamily = (FontFamily)Application.Current.Resources["KynxaUIFont"], FontSize = 14,
            Text = value, PlaceholderText = UiText.Get("输入聊天名称"), MaxLength = 80, MinWidth = 300
        };
        AutomationProperties.SetAutomationId(input, "ChatNameInput");
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot, Title = UiText.Get("重命名聊天"), Content = input,
            PrimaryButtonText = UiText.Get("保存"), CloseButtonText = UiText.Get("取消"), DefaultButton = ContentDialogButton.Primary,
            IsPrimaryButtonEnabled = !string.IsNullOrWhiteSpace(value)
        };
        input.TextChanged += (_, _) => dialog.IsPrimaryButtonEnabled = !string.IsNullOrWhiteSpace(input.Text);
        dialog.Opened += (_, _) => { input.Focus(FocusState.Programmatic); input.SelectAll(); };
        return await dialog.ShowAsync() == ContentDialogResult.Primary ? input.Text.Trim() : null;
    }
}
