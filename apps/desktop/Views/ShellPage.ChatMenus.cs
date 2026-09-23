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
        var chat = _standaloneChats.FirstOrDefault(c => c.Id == item.Id);
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

        Item(chat.IsPinned ? "取消置顶" : "置顶", "\uE718", () =>
        {
            chat.IsPinned = !chat.IsPinned;
            SaveChatChanges(project);
            return Task.CompletedTask;
        });
        Item("删除", "\uE74D", () =>
        {
            CaptureProjectDraft();
            CaptureStandaloneDraft();
            var chats = project?.Chats ?? _standaloneChats;
            int index = chats.IndexOf(chat);
            if (index < 0) return Task.CompletedTask;
            ClearActiveChat(chat);
            chats.RemoveAt(index);
            SaveChatChanges(project);
            CancelPendingReply(chat.Id);
            _undoSidebarChange = () =>
            {
                chats.Insert(Math.Min(index, chats.Count), chat);
                SaveChatChanges(project);
            };
            ShowChatNotice($"已删除“{chat.Title}”");
            return Task.CompletedTask;
        });
        Item("重命名", "\uE70F", async () =>
        {
            string? name = await AskChatNameAsync(chat.Title);
            if (name is null) return;
            chat.Title = name;
            if (_activeStandaloneChat == chat) _chatConversationTitle = name;
            if (_activeProjectChat == chat) _workConversationTitle = WorkChatTitle(project!, chat);
            SaveChatChanges(project);
            UpdateConversationTitle();
        });
        Item("归档", "\uE7B8", () =>
        {
            CaptureProjectDraft();
            CaptureStandaloneDraft();
            ClearActiveChat(chat);
            chat.IsArchived = true;
            SaveChatChanges(project);
            _undoSidebarChange = () =>
            {
                chat.IsArchived = false;
                SaveChatChanges(project);
            };
            ShowChatNotice($"已归档“{chat.Title}”");
            return Task.CompletedTask;
        });
        return menu;
    }

    private void SaveChatChanges(ProjectState? project)
    {
        CaptureProjectDraft();
        CaptureStandaloneDraft();
        if (project is null)
        {
            _projectStore.SaveChats(_standaloneChats);
            RebuildStandaloneRows();
        }
        else
        {
            _projectStore.Save(_projects);
            RenderProjects(project.Id);
        }
    }

    private void ClearActiveChat(ProjectChatState chat)
    {
        bool clearComposer = false;
        if (_activeProjectChat == chat)
        {
            _activeProjectChat = null;
            _workDraft = _workConversationTitle = string.Empty;
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
            Text = value, PlaceholderText = "输入聊天名称", MaxLength = 80, MinWidth = 300
        };
        AutomationProperties.SetAutomationId(input, "ChatNameInput");
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot, Title = "重命名聊天", Content = input,
            PrimaryButtonText = "保存", CloseButtonText = "取消", DefaultButton = ContentDialogButton.Primary,
            IsPrimaryButtonEnabled = !string.IsNullOrWhiteSpace(value)
        };
        input.TextChanged += (_, _) => dialog.IsPrimaryButtonEnabled = !string.IsNullOrWhiteSpace(input.Text);
        dialog.Opened += (_, _) => { input.Focus(FocusState.Programmatic); input.SelectAll(); };
        return await dialog.ShowAsync() == ContentDialogResult.Primary ? input.Text.Trim() : null;
    }
}
