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

    private void InitializeStandaloneChats()
    {
        _standaloneChats = _projectStore.ChatsExist
            ? _projectStore.LoadChats().Where(chat => chat.CanPersist).ToList()
            : ViewModel.RecentConversations.Select(chat => new ProjectChatState { Id = chat.Id, Title = chat.Title, IsSample = true }).ToList();
        _projectStore.SaveChats(_standaloneChats);
        RebuildStandaloneRows();
    }

    private void RebuildStandaloneRows()
    {
        ViewModel.RecentConversations.Clear();
        foreach (var chat in _standaloneChats.Where(chat => !chat.IsArchived).OrderByDescending(chat => chat.IsPinned))
            ViewModel.RecentConversations.Add(new RecentConversation(chat.Title, chat.IsSample ? "示例对话" : "刚刚") { Id = chat.Id });
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
        var chat = new ProjectChatState();
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

    private void UpdateConversationPresentation()
    {
        if (ConversationMessages is null) return;
        var chat = ViewModel.IsChatMode ? _activeStandaloneChat : _activeProjectChat;
        ActiveMessages.Clear();
        if (chat is not null)
        {
            foreach (var message in chat.Messages) ActiveMessages.Add(new ConversationMessageViewModel(chat.Id, message));
            if (_pendingReplies.TryGetValue(chat.Id, out var pending))
                ActiveMessages.Add(new ConversationMessageViewModel(chat.Id, error: pending.Error));
        }
        bool hasMessages = ActiveMessages.Count > 0;
        ConversationMessages.Visibility = hasMessages ? Visibility.Visible : Visibility.Collapsed;
        LogoHost.Visibility = hasMessages ? Visibility.Collapsed : Visibility.Visible;
        ChatAmbientLayer.Visibility = hasMessages ? Visibility.Collapsed : Visibility.Visible;
        MainContentHost.VerticalAlignment = hasMessages ? VerticalAlignment.Bottom : VerticalAlignment.Center;
        MainContentHost.Margin = hasMessages ? new Thickness(0, 0, 0, 16) : new Thickness(0);
        ApplyLayout();
        UpdateSendButtonState();
        if (hasMessages)
        {
            var lastMessage = ActiveMessages.Last();
            DispatcherQueue.TryEnqueue(() =>
            {
                if (!_chatClosing && ConversationMessages.Visibility == Visibility.Visible && ActiveMessages.Contains(lastMessage))
                    ConversationMessages.ScrollIntoView(lastMessage);
            });
        }
    }

    private async void SendButton_Click(object sender, RoutedEventArgs e) => await SendPromptAsync();

    private async Task SendPromptAsync()
    {
        if (_sendingPrompt || IsReplyInProgress(ActiveChatId)) return;
        string text = PromptTextBox.Text.Trim();
        if (text.Length == 0) { PromptTextBox.Focus(FocusState.Programmatic); return; }
        _sendingPrompt = true;
        UpdateSendButtonState();
        PendingChatReply? preparedReply = null;
        try
        {
            await RunProjectActionAsync(() =>
            {
                ProjectChatState chat;
                if (ViewModel.IsChatMode)
                {
                    chat = _activeStandaloneChat ?? new ProjectChatState();
                    if (_activeStandaloneChat is null) _standaloneChats.Insert(0, chat);
                    _activeStandaloneChat = chat;
                }
                else
                {
                    if (_activeProjectChat is null)
                    {
                        if (!_workWithoutFolder)
                        {
                            ShowWorkspacePicker();
                            return Task.CompletedTask;
                        }
                        var project = GetFolderlessWorkspace();
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
                if (ViewModel.IsChatMode)
                {
                    _projectStore.SaveChats(_standaloneChats);
                    _chatConversationTitle = chat.Title;
                    _chatDraft = string.Empty;
                    RebuildStandaloneRows();
                }
                else
                {
                    var project = _projects.First(p => p.Chats.Contains(chat));
                    _projectStore.Save(_projects);
                    _workConversationTitle = WorkChatTitle(project, chat);
                    _workDraft = string.Empty;
                    RenderProjects(project.Id);
                }
                PromptTextBox.Text = ViewModel.Prompt = string.Empty;
                preparedReply = BeginPendingReply(chat.Id, text, _selectedModel?.ProviderId, _selectedModel?.ModelId);
                UpdateConversationTitle();
                UpdateConversationPresentation();
                return Task.CompletedTask;
            });
            if (preparedReply is not null) await ReceiveMockReplyAsync(preparedReply);
        }
        finally
        {
            _sendingPrompt = false;
            UpdateSendButtonState();
        }
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
    { if (!ContainsKeyboardFocus(ChatHeader)) { NewChatButton.Opacity = 0; NewChatButton.IsHitTestVisible = false; } });
}
