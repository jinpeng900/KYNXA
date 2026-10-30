using System.Text;
using KYNXA_Desktop.Controls;
using Microsoft.UI.Xaml;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private void InitializeExpansionWorkbench()
    {
        ExpansionRemoteService.ModelSelection = () => (_selectedModel?.ProviderId, _selectedModel?.ModelId);
        DocumentPanel.SelectionRequested += (_, selection) =>
        {
            if (_chatClosing || selection.ConversationId != ActiveChatId) return;
            var context = new StringBuilder("[引用：").Append(selection.Title).Append(" · ").Append(selection.Kind);
            if (!string.IsNullOrEmpty(selection.ObjectId)) context.Append(" · 对象 ").Append(selection.ObjectId);
            if (!string.IsNullOrEmpty(selection.Version)) context.Append(" · 版本 ").Append(selection.Version);
            context.AppendLine("]");
            if (!string.IsNullOrWhiteSpace(selection.Path) && !selection.Path.StartsWith("expansion-workbench:", StringComparison.Ordinal))
                context.Append("文件：").AppendLine(selection.Path);
            foreach (string line in selection.Text.Replace("\r\n", "\n").Split('\n')) context.Append("> ").AppendLine(line);
            PromptTextBox.Text = ViewModel.Prompt = PromptTextBox.Text.TrimEnd() + (PromptTextBox.Text.Length == 0 ? "" : "\n\n") + context;
            PromptTextBox.SelectionStart = PromptTextBox.Text.Length;
            PromptTextBox.Focus(FocusState.Programmatic);
            ShowActionFeedback("已将所选内容添加到输入框，编辑后发送。");
        };
        DocumentPanel.WorkbenchRequested += (_, _) => OpenExpansionWorkbench();
        DocumentPanel.ConversationRequested += (_, id) =>
        {
            if (_chatClosing || HasPresentationModal()) return;
            var (chat, project) = FindChat(id);
            if (chat is null) { ShowActionFeedback("此会话不在当前会话目录中。"); return; }
            if (project is not null) SelectProjectChat(project, chat);
            else { SetPrimaryMode(true, updateConversation: false); SelectStandaloneChat(new RecentConversation(chat.Title, "") { Id = id }); }
        };
        DocumentPanel.WorkbenchActionRequested += (_, action) =>
        {
            if (_chatClosing || HasPresentationModal()) return;
            switch (action)
            {
                case "attach": ComposerAddButton_Click(this, new RoutedEventArgs()); break;
                case "folder": UseExistingProjectFolder_Click(this, new RoutedEventArgs()); break;
                case "files": OpenWorkspaceFile(); break;
                case "terminal": OpenWorkspaceTerminal(); break;
                case "extensions": OpenAgentTools(); break;
                case "settings": StorageSettings_Click(this, new RoutedEventArgs()); break;
                case "keyboard": _ = ShowFeatureInfoAsync("键盘操作", "Enter 发送，Shift+Enter 换行；Ctrl+F 查找正文，Ctrl+Shift+K 搜索会话，Ctrl+L 聚焦输入，Ctrl+N 新建聊天。将文件拖到输入框可添加附件；所选预览内容可引用到输入框。"); break;
                case "notifications": SetCompletionNotifications(true); break;
                case "notifications-off": SetCompletionNotifications(false); break;
                case "remote-start": _ = StartExpansionRemoteAsync(); break;
                case "remote-stop": _ = StopExpansionRemoteAsync(); break;
            }
        };
    }

    private void ExpansionWorkbench_Click(object sender, RoutedEventArgs args) => OpenExpansionWorkbench();

    private async void OpenExpansionWorkbench()
    {
        if (_chatClosing || !_projectsReady || HasPresentationModal()) return;
        if (ActiveChatId is null) StartNewChatFromNavigation();
        if (ActiveChatId is not Guid conversation) return;
        var (chat, project) = FindChat(conversation);
        if (chat is null) return;
        try
        {
            // A workspace needs a durable conversation scope even before its first prompt.
            chat.KeepWorkspace = true;
            if (!chat.IsPersisted) await SaveChatChangesAsync(project);
            if (_chatClosing || ActiveChatId != conversation) return;
            var mount = CurrentWorkspaceMount();
            string target = "expansion-workbench:" + mount?.Id.ToString("D");
            AddWorkspaceTab(conversation, new($"workbench/{conversation:D}/{mount?.Id:D}", "workbench", "工作台",
                target, "1", true, WorkspacePresentationPriority.Requested, mount?.Id), select: true);
        }
        catch (Exception error) when (error is IOException or HttpRequestException or InvalidOperationException)
        { if (!_chatClosing) ShowActionFeedback("工作台打开失败：" + error.Message); }
    }

    private void ToggleCompletionNotifications()
        => SetCompletionNotifications(!ExpansionNotifications.Enabled);

    private void SetCompletionNotifications(bool enabled)
    {
        bool saved = TrySetCompletionNotifications(enabled);
        ShowActionFeedback(UiText.Get(saved
            ? enabled ? "已开启本机回复完成通知。" : "已关闭本机回复完成通知。"
            : "通知偏好未能保存，请稍后重试。"));
    }

    private bool TrySetCompletionNotifications(bool enabled)
    {
        try
        {
            ExpansionNotifications.SetEnabled(enabled);
            _refreshCompletionNotificationSettings?.Invoke();
            return true;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            _refreshCompletionNotificationSettings?.Invoke();
            return false;
        }
    }

    private async Task StartExpansionRemoteAsync()
    {
        if (ActiveChatId is not Guid conversation) return;
        try { await ExpansionRemoteService.StartAsync(conversation); ShowActionFeedback("手机连接已开启，点击刷新连接状态获取配对地址。"); }
        catch (Exception error) { if (!_chatClosing) ShowActionFeedback("手机连接未开启：" + error.Message); }
    }

    private async Task StopExpansionRemoteAsync()
    {
        try
        {
            await ExpansionRemoteService.StopAsync();
            if (!_chatClosing) ShowActionFeedback("手机连接服务已关闭。");
        }
        catch (Exception error) { if (!_chatClosing) ShowActionFeedback("手机连接关闭失败：" + error.Message); }
    }
}
