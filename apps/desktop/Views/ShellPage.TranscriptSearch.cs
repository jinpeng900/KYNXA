using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.System;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private void OpenTranscriptSearch()
    {
        if (_promptCompositionActive || HasPresentationModal() || _chatClosing || !IsLoaded) return;
        _historySearchFlyout?.Hide();
        // Finding text never opens a collapsed sidebar or changes the active conversation.
        // 正文查找不展开已收起的侧栏，也不改变当前会话。
        ConversationMessages.Visibility = Visibility.Visible;
        ConversationMessages.OpenFindText();
    }

    private void TranscriptSearchShortcut_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        if (_promptCompositionActive || HasPresentationModal()) return;
        OpenTranscriptSearch();
        args.Handled = true;
    }

    private bool CanUseNativeTranscriptSearchShortcut() => !_promptCompositionActive && !HasPresentationModal()
        && !_chatClosing && IsLoaded && ConversationMessages.IsFindTextOpen && XamlRoot is not null
        && !VisualTreeHelper.GetOpenPopupsForXamlRoot(XamlRoot).Any(popup => popup.IsOpen);

    private void TranscriptSearchNavigation_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        // The WebView owns F3 while focused so its current IME composition can veto navigation.
        // WebView 持有焦点时自行处理 F3，使当前输入法组合状态仍能阻止导航。
        if (!CanUseNativeTranscriptSearchShortcut() || ConversationMessages.Browser.FocusState != FocusState.Unfocused) return;
        ConversationMessages.NavigateFindText(sender.Modifiers.HasFlag(VirtualKeyModifiers.Shift));
        args.Handled = true;
    }

    private void TranscriptSearchClose_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        // Dismiss the top native drawer first, including when focus is inside its embedded WebView.
        // 优先关闭最上层原生覆盖栏，嵌入 WebView 获得焦点时也可使用同一 Escape 入口。
        if (TryDismissCompactSidebar() || TryDismissCompactWorkPanel()) { args.Handled = true; return; }
        // Browser Escape owns its menus and diagram modal; native controls can close the underlying find bar themselves.
        // 浏览器自行处理其菜单和图表模态的 Escape；焦点位于原生控件时才由宿主关闭正文查找。
        if (!CanUseNativeTranscriptSearchShortcut() || ConversationMessages.Browser.FocusState != FocusState.Unfocused) return;
        ConversationMessages.CloseFindText();
        args.Handled = true;
    }

    private void OpenConversationSearch()
    {
        if (_promptCompositionActive || HasPresentationModal() || _chatClosing || !IsLoaded) return;
        ShowHistorySearch(HistorySearchButton.Visibility == Visibility.Visible ? HistorySearchButton : CompactSidebarButton);
    }

    private void Transcript_ConversationSearchRequested(object? sender, EventArgs e) => OpenConversationSearch();

    private void Transcript_FindTextStateChanged(object? sender, EventArgs e)
    {
        if (_chatClosing || !IsLoaded || ActiveMessages.Count > 0) return;
        bool open = ConversationMessages.IsFindTextOpen;
        // An empty draft still has a searchable surface; closing it restores the centered home composer.
        // 空草稿也提供可查找的区域；关闭查找后恢复居中的首页输入区。
        if (!open && !HasPresentationModal() && ConversationMessages.Browser.FocusState != FocusState.Unfocused)
            PromptTextBox.Focus(FocusState.Programmatic);
        PositionConversationComposer(ViewModel.IsChatMode ? _activeStandaloneChat : _activeProjectChat);
    }

    private void Transcript_FindTextClosed(object? sender, EventArgs e)
    {
        // Explicit close returns focus only when the browser still owns it; late messages cannot steal a native dialog's focus.
        // 仅在浏览器仍持有焦点时由显式关闭返回输入框；晚到消息不能夺取原生弹窗焦点。
        if (_chatClosing || !IsLoaded || HasPresentationModal() || ConversationMessages.Browser.FocusState == FocusState.Unfocused) return;
        PromptTextBox.Focus(FocusState.Programmatic);
    }
}
