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

    private void PromptTextBox_TextCompositionStarted(TextBox sender, TextCompositionStartedEventArgs args) =>
        _promptCompositionActive = true;

    private void PromptTextBox_TextCompositionEnded(TextBox sender, TextCompositionEndedEventArgs args)
    {
        _promptCompositionActive = false;
        // Some IMEs finish composition before forwarding the confirming Enter key.
        _suppressComposingEnter = true;
    }

    private async void PromptTextBox_KeyDown(object sender, KeyRoutedEventArgs e)
    {
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
        ViewModel.RecentConversations.Clear();
        foreach (var chat in _standaloneChats.Where(chat => !chat.IsArchived).OrderByDescending(chat => chat.IsPinned))
            ViewModel.RecentConversations.Add(new RecentConversation(chat.Title, chat.IsSample ? UiText.Get("示例对话") : UiText.Get("刚刚")) { Id = chat.Id });
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
        _chatDraft = chat.Draft;
        _chatConversationTitle = chat.Title;
        PromptTextBox.Text = ViewModel.Prompt = chat.Draft;
        UpdateConversationTitle();
        UpdateConversationPresentation();
        PromptTextBox.Focus(FocusState.Programmatic);
    }

    private void UpdateConversationPresentation(bool openAtBottom = true)
    {
        if (ConversationMessages is null) return;
        var chat = ViewModel.IsChatMode ? _activeStandaloneChat : _activeProjectChat;
        ActiveMessages.Clear();
        if (chat is not null)
        {
            foreach (var message in chat.Messages)
            {
                var row = _pendingReplies.TryGetValue(chat.Id, out var pending) && pending.Message == message
                    ? pending.Presentation : new ConversationMessageViewModel(chat.Id, message);
                row.SetRetryAllowed(message == chat.Messages.LastOrDefault());
                ActiveMessages.Add(row);
            }
        }
        bool hasMessages = ActiveMessages.Count > 0;
        ConversationMessages.Visibility = hasMessages ? Visibility.Visible : Visibility.Collapsed;
        LogoHost.Visibility = hasMessages ? Visibility.Collapsed : Visibility.Visible;
        ChatAmbientLayer.Visibility = hasMessages ? Visibility.Collapsed : Visibility.Visible;
        MainContentHost.VerticalAlignment = hasMessages ? VerticalAlignment.Bottom : VerticalAlignment.Center;
        MainContentHost.Margin = hasMessages ? new Thickness(0, 0, 0, 16) : new Thickness(0);
        ScreenshotPanel.ShowConversation(chat?.Id, ActiveMessages.ToArray());
        UpdateMountedWorkspacePresentation();
        ApplyLayout();
        UpdateSendButtonState();
        ConversationMessages.ShowConversation(chat?.Id, ActiveMessages.ToArray(), openAtBottom);
    }

    private async void SendButton_Click(object sender, RoutedEventArgs e)
    {
        if (ActiveChatId is Guid id && _pendingReplies.TryGetValue(id, out var pending))
        {
            pending.Cancellation.Cancel();
            return;
        }
        await SendPromptAsync();
    }

    private async Task SendPromptAsync()
    {
        if (_sendingPrompt || IsReplyInProgress(ActiveChatId)) return;
        string originalDraft = PromptTextBox.Text;
        string text = originalDraft.Trim();
        bool chatMode = ViewModel.IsChatMode;
        string? selectedProvider = _selectedModel?.ProviderId, selectedModel = _selectedModel?.ModelId;
        if (text.Length == 0) { PromptTextBox.Focus(FocusState.Programmatic); return; }
        ConversationMessages.BeforeSend();
        _sendingPrompt = true;
        UpdateSendButtonState();
        PendingChatReply? preparedReply = null;
        try
        {
            await RunProjectActionAsync(async () =>
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
                            return;
                        }
                        var project = selectedProject ?? GetFolderlessWorkspace();
                        _activeProjectChat = new ProjectChatState();
                        project.Chats.Insert(0, _activeProjectChat);
                    }
                    chat = _activeProjectChat;
                }
                if (!chat.CanPersist)
                {
                    string title = System.Text.RegularExpressions.Regex.Replace(text, @"\s+", " ");
                    chat.Title = title.Length > 24 ? title[..24] + "…" : title;
                }
                chat.Messages.Add(new ChatMessageState { Role = "user", Content = text });
                chat.Draft = string.Empty;
                // First submitted message is the commit boundary; draft-only chats are filtered by the store.
                if (chatMode)
                {
                    await _projectStore.SaveChatsAsync(_standaloneChats);
                    if (_activeStandaloneChat == chat)
                    {
                        _chatConversationTitle = chat.Title;
                        _chatDraft = string.Empty;
                    }
                    RebuildStandaloneRows();
                }
                else
                {
                    var project = _projects.First(p => p.Chats.Contains(chat));
                    _selectedWorkProjectId = project.IsFolderlessWorkspace ? null : project.Id;
                    _workChatToReveal = chat.Id;
                    KYNXA_Desktop.Services.ProjectOrdering.Activate(_projects, project);
                    KYNXA_Desktop.Services.WorkSidebarState.ActivateChat(project, chat);
                    await _projectStore.SaveAsync(_projects);
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
