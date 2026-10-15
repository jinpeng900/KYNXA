using KYNXA_Desktop.Services;
using System.Collections.ObjectModel;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Input;
using Windows.System;
using Windows.UI.Core;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private List<ProjectChatState> _standaloneChats = [];
    private ProjectChatState? _activeStandaloneChat;
    public ObservableCollection<ConversationMessageViewModel> ActiveMessages { get; } = [];
    private bool _promptCompositionActive;
    private bool _suppressComposingEnter;
    private bool _sendingPrompt;
    private bool UsesConversationLayout => (ViewModel.IsChatMode ? _activeStandaloneChat : _activeProjectChat) is { } chat
        && (chat.IsPersisted || chat.Messages.Count > 0);

    private void PositionConversationComposer(ProjectChatState? chat)
    {
        // Persisted chat identity is available before its paged body; it must not use the centered new-chat layout.
        // 正式聊天身份先于分页正文到达；已有聊天不能因为正文仍在读取就使用居中的新聊天布局。
        bool conversationLayout = chat is not null && (chat.IsPersisted || chat.Messages.Count > 0);
        ConversationMessages.Visibility = conversationLayout || ConversationMessages.IsFindTextOpen ? Visibility.Visible : Visibility.Collapsed;
        LogoHost.Visibility = conversationLayout ? Visibility.Collapsed : Visibility.Visible;
        ChatAmbientLayer.Visibility = conversationLayout ? Visibility.Collapsed : Visibility.Visible;
        MainContentHost.VerticalAlignment = conversationLayout ? VerticalAlignment.Bottom : VerticalAlignment.Center;
        MainContentHost.Margin = conversationLayout ? new Thickness(0, 0, 0, 16) : new Thickness(0);
        if (conversationLayout) MainContentHost.Translation = System.Numerics.Vector3.Zero;
    }

    private void PromptTextBox_TextCompositionStarted(TextBox sender, TextCompositionStartedEventArgs args) =>
        _promptCompositionActive = true;

    private void PromptTextBox_TextCompositionEnded(TextBox sender, TextCompositionEndedEventArgs args)
    {
        _promptCompositionActive = false;
        // Some IMEs finish composition before forwarding the confirming Enter key.
        // 某些输入法在转发确认用的 Enter 键之前就结束组合输入。
        _suppressComposingEnter = true;
    }

    private async void PromptTextBox_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == VirtualKey.V && (InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Control) & CoreVirtualKeyStates.Down) != 0
            && ClipboardContainsAttachments())
        {
            e.Handled = true;
            try { await TryPasteAttachmentsAsync(); }
            catch (Exception error) when (error is System.Runtime.InteropServices.COMException or IOException or UnauthorizedAccessException or InvalidOperationException)
            { if (!_chatClosing) ShowActionFeedback(UiText.Get("附件粘贴失败，请重试。")); }
            return;
        }
        if (e.Key != VirtualKey.Enter)
        {
            _suppressComposingEnter = false;
            return;
        }

        if (_promptCompositionActive || _suppressComposingEnter)
        {
            _suppressComposingEnter = false;
            return;
        }

        if ((InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Shift) & CoreVirtualKeyStates.Down) != 0)
            return;

        e.Handled = true;
        await SendPromptAsync();
    }

    private void PromptTextBox_KeyUp(object sender, KeyRoutedEventArgs e)
    {
        if (!_promptCompositionActive) _suppressComposingEnter = false;
    }

    private void RebuildStandaloneRows()
    {
        // Preserve stable row identities and let ListView virtualize containers instead of rebuilding every chat.
        // 保留稳定行身份，让 ListView 虚拟化容器，避免每次刷新重建全部聊天行。
        var rows = ViewModel.RecentConversations;
        var desired = _standaloneChats.Where(chat => !chat.IsArchived).OrderByDescending(chat => chat.IsPinned).ToList();
        var activeIds = desired.Select(chat => chat.Id).ToHashSet();
        for (int index = rows.Count - 1; index >= 0; index--)
            if (!activeIds.Contains(rows[index].Id)) rows.RemoveAt(index);
        var positions = rows.ToDictionary(row => row.Id);
        for (int index = 0; index < desired.Count; index++)
        {
            var chat = desired[index];
            string detail = chat.IsSample ? UiText.Get("示例对话") : UiText.Get("刚刚");
            if (!positions.TryGetValue(chat.Id, out var row))
            {
                rows.Insert(index, new RecentConversation(chat.Title, detail) { Id = chat.Id });
                continue;
            }
            int currentIndex = rows[index].Id == chat.Id ? index : rows.IndexOf(row);
            if (currentIndex != index) rows.Move(currentIndex, index);
            if (row.Title != chat.Title || row.RelativeTime != detail)
                rows[index] = new RecentConversation(chat.Title, detail) { Id = chat.Id };
        }
        ChatHistoryList.SelectedItem = ViewModel.RecentConversations.FirstOrDefault(chat => chat.Id == _activeStandaloneChat?.Id);
    }

    private void CaptureStandaloneDraft()
    {
        if (ViewModel.IsChatMode && _activeStandaloneChat is not null) _activeStandaloneChat.Draft = PromptTextBox.Text;
    }

    private void DiscardEmptyProjectChats(Guid? except = null)
    {
        bool changed = false;
        foreach (var project in _projects)
            changed |= project.Chats.RemoveAll(chat => !chat.CanPersist && chat.Id != except) > 0;
        if (_activeProjectChat is { CanPersist: false } active && active.Id != except)
        {
            _activeProjectChat = null;
            _workDraft = _workConversationTitle = string.Empty;
        }
        if (changed) RenderProjects();
    }

    private void DiscardEmptyStandaloneChats(Guid? except = null)
    {
        bool changed = _standaloneChats.RemoveAll(chat => !chat.CanPersist && chat.Id != except) > 0;
        if (_activeStandaloneChat is { CanPersist: false } active && active.Id != except)
        {
            _activeStandaloneChat = null;
            _chatDraft = _chatConversationTitle = string.Empty;
        }
        if (changed) RebuildStandaloneRows();
    }

    private void NewStandaloneChat_Click(object sender, RoutedEventArgs e)
    {
        CaptureStandaloneDraft();
        DiscardEmptyStandaloneChats();
        var chat = new ProjectChatState { Title = UiText.Get("新聊天") };
        _standaloneChats.Insert(0, chat);
        _activeStandaloneChat = chat;
        _chatConversationTitle = chat.Title;
        _chatDraft = string.Empty;
        PromptTextBox.Text = ViewModel.Prompt = string.Empty;
        RebuildStandaloneRows();
        UpdateConversationTitle();
        UpdateConversationPresentation();
        PromptTextBox.Focus(FocusState.Programmatic);
    }

    private void SelectStandaloneChat(RecentConversation item)
    {
        var chat = _standaloneChats.FirstOrDefault(chat => chat.Id == item.Id);
        if (chat is null) return;
        CaptureStandaloneDraft();
        DiscardEmptyStandaloneChats(chat.Id);
        _activeStandaloneChat = chat;
        PositionConversationComposer(chat);
        _chatDraft = chat.Draft;
        _chatConversationTitle = chat.Title;
        PromptTextBox.Text = ViewModel.Prompt = chat.Draft;
        UpdateConversationTitle();
        UpdateConversationPresentation();
        PromptTextBox.Focus(FocusState.Programmatic);
    }

    private void UpdateConversationPresentation(bool openAtBottom = false)
    {
        NotifyApprovalConversationChanged();
        if (ConversationMessages is null) return;
        var chat = ViewModel.IsChatMode ? _activeStandaloneChat : _activeProjectChat;
        PositionConversationComposer(chat);
        if (chat is not null) _ = EnsureRecentHistoryAsync(chat);
        else _historyConversationId = null;
        if (chat is null || ActiveMessages.Count > 0 && ActiveMessages[0].ConversationId != chat.Id)
            ActiveMessages.Clear();
        if (chat is not null)
        {
            // Reuse unchanged rows when history or reply state refreshes the current conversation.
            // 当前聊天的历史或回复状态更新时复用未变行，避免清空集合后重建全部消息。
            var existing = ActiveMessages.ToDictionary(row => row.Message.Id);
            var desired = new List<ConversationMessageViewModel>(chat.Messages.Count);
            var lastMessage = chat.Messages.LastOrDefault();
            foreach (var message in chat.Messages)
            {
                var row = _pendingReplies.TryGetValue(chat.Id, out var pending) && pending.Message == message
                    ? pending.Presentation : existing.TryGetValue(message.Id, out var previous) && previous.Message == message
                        ? previous : new ConversationMessageViewModel(chat.Id, message);
                row.SetRetryAllowed(message == lastMessage);
                desired.Add(row);
            }
            for (int index = 0; index < desired.Count; index++)
            {
                var row = desired[index];
                if (index < ActiveMessages.Count && ActiveMessages[index] == row) continue;
                int previousIndex = ActiveMessages.IndexOf(row);
                if (previousIndex >= 0) ActiveMessages.Move(previousIndex, index);
                else ActiveMessages.Insert(index, row);
            }
            while (ActiveMessages.Count > desired.Count) ActiveMessages.RemoveAt(ActiveMessages.Count - 1);
        }
        var messageRows = ActiveMessages.ToArray();
        ConversationMessages.ShowConversation(chat?.Id, messageRows, openAtBottom);
        if (chat is not null && _historyPages.TryGetValue(chat.Id, out var history)) PublishHistoryAvailability(history);
        ScreenshotPanel.ShowConversation(chat?.Id, messageRows);
        RefreshAttachmentDrafts();
        RefreshPromptQueue();
        UpdateMountedWorkspacePresentation();
        UpdateSendButtonState();
    }

    private async void SendButton_Click(object sender, RoutedEventArgs e)
    {
        if (ActiveChatId is Guid id && _pendingReplies.TryGetValue(id, out var pending))
        {
            if (!string.IsNullOrWhiteSpace(PromptTextBox.Text) || HasAttachmentDrafts) TryQueuePrompt();
            else await StopPendingReplyAsync(pending);
            return;
        }
        await SendPromptAsync();
    }

    private async Task SendPromptAsync()
    {
        if (TryQueuePrompt()) return;
        if (_sendingPrompt || _savingOnClose || _chatClosing || IsReplyInProgress(ActiveChatId)) return;
        if (await TrySteerPausedTaskAsync()) return;
        string originalDraft = PromptTextBox.Text;
        string text = originalDraft.Trim();
        string attachmentDraftKey = AttachmentDraftKey;
        var attachmentDrafts = AttachmentDraftsFor(attachmentDraftKey).ToArray();
        bool chatMode = ViewModel.IsChatMode;
        string? selectedProvider = _selectedModel?.ProviderId, selectedModel = _selectedModel?.ModelId;
        if (text.Length == 0 && attachmentDrafts.Length == 0) { PromptTextBox.Focus(FocusState.Programmatic); return; }
        ConversationMessages.BeforeSend();
        _sendingPrompt = true;
        UpdateSendButtonState();
        PendingChatReply? preparedReply = null;
        try
        {
            await RunProjectActionAsync(() =>
            {
                ProjectChatState chat;
                if (chatMode)
                {
                    chat = _activeStandaloneChat ?? new ProjectChatState();
                    if (_activeStandaloneChat is null) _standaloneChats.Insert(0, chat);
                    _activeStandaloneChat = chat;
                }
                else
                {
                    if (_activeProjectChat is null)
                    {
                        var selectedProject = KYNXA_Desktop.Services.WorkSidebarState.FindSelectedProject(_projects, _selectedWorkProjectId);
                        if (selectedProject is null && !_workWithoutFolder)
                        {
                            ShowWorkspacePicker();
                            return Task.CompletedTask;
                        }
                        var project = selectedProject ?? GetFolderlessWorkspace();
                        _activeProjectChat = new ProjectChatState();
                        project.Chats.Insert(0, _activeProjectChat);
                    }
                    chat = _activeProjectChat;
                }
                if (!chat.CanPersist)
                {
                    string title = System.Text.RegularExpressions.Regex.Replace(text.Length == 0 ? attachmentDrafts[0].Name : text, @"\s+", " ");
                    chat.Title = title.Length > 24 ? title[..24] + "…" : title;
                }
                var user = new ChatMessageState { Role = "user", Content = text,
                    Attachments = attachmentDrafts.Select(file => file.ToAttachment()).ToList() };
                chat.Messages.Add(user);
                if (attachmentDrafts.Length > 0)
                {
                    _messageAttachmentDrafts[user.Id] = attachmentDrafts;
                    var ids = attachmentDrafts.Select(file => file.Id).ToHashSet();
                    AttachmentDraftsFor(attachmentDraftKey).RemoveAll(file => ids.Contains(file.Id));
                }
                chat.Draft = string.Empty;
                // Present the accepted input before storage, gateway startup or model admission can wait.
                // 点击发送即展示用户消息与回复占位；保存、网关启动和模型准入的等待不阻塞展示。
                if (chatMode)
                {
                    if (_activeStandaloneChat == chat)
                    {
                        _chatConversationTitle = chat.Title;
                        _chatDraft = string.Empty;
                    }
                    RebuildStandaloneRows();
                }
                else
                {
                    var project = _projects.First(candidateProject => candidateProject.Chats.Contains(chat));
                    _selectedWorkProjectId = project.IsFolderlessWorkspace ? null : project.Id;
                    _workChatToReveal = chat.Id;
                    KYNXA_Desktop.Services.ProjectOrdering.Activate(_projects, project);
                    KYNXA_Desktop.Services.WorkSidebarState.ActivateChat(project, chat);
                    RecordWorkChatActivity(chat);
                    if (_activeProjectChat == chat)
                    {
                        _workConversationTitle = WorkChatTitle(project, chat);
                        _workDraft = string.Empty;
                    }
                    RenderProjects();
                }
                if (ViewModel.IsChatMode == chatMode && ActiveChatId == chat.Id && PromptTextBox.Text == originalDraft)
                    PromptTextBox.Text = ViewModel.Prompt = string.Empty;
                preparedReply = BeginPendingReply(chat.Id, text, selectedProvider, selectedModel);
                UpdateConversationTitle();
                UpdateConversationPresentation(openAtBottom: false);
                return Task.CompletedTask;
            });
        }
        finally
        {
            _sendingPrompt = false;
            UpdateSendButtonState();
        }
        if (preparedReply is not null) await ReceiveReplyAsync(preparedReply);
    }

    private void ChatHeader_PointerEntered(object sender, PointerRoutedEventArgs e)
    { NewChatButton.Opacity = 1; NewChatButton.IsHitTestVisible = true; }
    private void ChatHeader_PointerExited(object sender, PointerRoutedEventArgs e)
    {
        bool focus = ContainsKeyboardFocus(ChatHeader);
        NewChatButton.Opacity = focus ? 1 : 0;
        NewChatButton.IsHitTestVisible = focus;
    }
    private void ChatHeader_GotFocus(object sender, RoutedEventArgs e)
    { if (ContainsKeyboardFocus(ChatHeader)) { NewChatButton.Opacity = 1; NewChatButton.IsHitTestVisible = true; } }
    private void ChatHeader_LostFocus(object sender, RoutedEventArgs e) => DispatcherQueue.TryEnqueue(() =>
    {
        if (_projectViewClosed || !ChatHeader.IsLoaded) return;
        if (!ContainsKeyboardFocus(ChatHeader)) { NewChatButton.Opacity = 0; NewChatButton.IsHitTestVisible = false; }
    });
}
