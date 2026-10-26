using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Layout;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;
using Windows.Storage.Pickers;
using Windows.System;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private DispatcherTimer? _actionFeedbackTimer;
    private bool _presentationDialogOpen;
    private bool _isCompactLayout;
    private bool _temporarySidebarOpen;

    private void InitializePresentationActions()
    {
        AutomationProperties.SetLiveSetting(ActionFeedbackText, Microsoft.UI.Xaml.Automation.Peers.AutomationLiveSetting.Polite);
        // Page-wide shortcuts must not create an automatic tooltip over the transcript.
        // 页面级快捷键不在聊天正文上生成自动提示；按钮自己的说明提示继续保留。
        KeyboardAcceleratorPlacementMode = Microsoft.UI.Xaml.Input.KeyboardAcceleratorPlacementMode.Hidden;
        // Register SDK key enums in code so the XAML compiler need not inspect their metadata.
        // 在代码中注册 SDK 按键枚举，避免 XAML 编译器解析其元数据。
        var findText = new KeyboardAccelerator { Key = VirtualKey.F, Modifiers = VirtualKeyModifiers.Control };
        findText.Invoked += TranscriptSearchShortcut_Invoked;
        var findNext = new KeyboardAccelerator { Key = VirtualKey.F3 };
        var findPrevious = new KeyboardAccelerator { Key = VirtualKey.F3, Modifiers = VirtualKeyModifiers.Shift };
        findNext.Invoked += TranscriptSearchNavigation_Invoked;
        findPrevious.Invoked += TranscriptSearchNavigation_Invoked;
        var closeFind = new KeyboardAccelerator { Key = VirtualKey.Escape };
        closeFind.Invoked += TranscriptSearchClose_Invoked;
        // Use Ctrl+Shift+K because Chinese input may intercept Ctrl+Shift+F before WebView.
        // 中文输入可能先拦截 Ctrl+Shift+F，因此会话搜索使用 Ctrl+Shift+K。
        var search = new KeyboardAccelerator { Key = VirtualKey.K, Modifiers = VirtualKeyModifiers.Control | VirtualKeyModifiers.Shift };
        search.Invoked += SearchShortcut_Invoked;
        var composer = new KeyboardAccelerator { Key = VirtualKey.L, Modifiers = VirtualKeyModifiers.Control };
        composer.Invoked += ComposerShortcut_Invoked;
        var newChat = new KeyboardAccelerator { Key = VirtualKey.N, Modifiers = VirtualKeyModifiers.Control };
        newChat.Invoked += NewChatShortcut_Invoked;
        var help = new KeyboardAccelerator { Key = VirtualKey.F1 };
        help.Invoked += HelpShortcut_Invoked;
        KeyboardAccelerators.Add(findText);
        KeyboardAccelerators.Add(findNext);
        KeyboardAccelerators.Add(findPrevious);
        KeyboardAccelerators.Add(closeFind);
        KeyboardAccelerators.Add(search);
        KeyboardAccelerators.Add(composer);
        KeyboardAccelerators.Add(newChat);
        KeyboardAccelerators.Add(help);
        KeyDown += Presentation_KeyDown;
        ConversationMessages.ConversationSearchRequested += Transcript_ConversationSearchRequested;
        ConversationMessages.FindTextClosed += Transcript_FindTextClosed;
        ConversationMessages.FindTextStateChanged += Transcript_FindTextStateChanged;
        Unloaded += (_, _) =>
        {
            ClearActionFeedback();
        };
    }

    private void ShowActionFeedback(string text)
    {
        if (_chatClosing || !IsLoaded) return;
        ActionFeedbackText.Text = text;
        ActionFeedbackHost.Visibility = Visibility.Visible;
        _actionFeedbackTimer ??= new DispatcherTimer { Interval = TimeSpan.FromSeconds(3) };
        _actionFeedbackTimer.Tick -= ActionFeedbackTimer_Tick;
        _actionFeedbackTimer.Tick += ActionFeedbackTimer_Tick;
        _actionFeedbackTimer.Stop();
        _actionFeedbackTimer.Start();
    }

    private void ActionFeedbackTimer_Tick(object? sender, object args) => ClearActionFeedback();

    private void ClearActionFeedback()
    {
        _actionFeedbackTimer?.Stop();
        if (ActionFeedbackHost is not null) ActionFeedbackHost.Visibility = Visibility.Collapsed;
    }

    private void ApplySidebarPresentation()
    {
        _isCompactLayout = ShellGrid.ActualWidth is > 0 and < ShellLayoutMetrics.CompactSidebarBreakpoint;
        if (!_isCompactLayout) _temporarySidebarOpen = false;
        bool expanded = _isCompactLayout ? _temporarySidebarOpen : !_layout.SidebarCollapsed;
        double preferredWidth = Math.Clamp(_layout.SidebarWidth, ShellLayoutMetrics.SidebarMin, ShellLayoutMetrics.SidebarMax);
        // A temporary drawer never changes the user's saved sidebar width or collapse preference.
        // 临时抽屉不改变用户保存的侧栏宽度或折叠偏好。
        SidebarColumn.Width = new GridLength(_isCompactLayout || !expanded ? ShellLayoutMetrics.SidebarCollapsed : preferredWidth);
        // Trim only the reserved gutter so the existing footer icons have equal outer margins without moving them.
        // 仅缩小额外预留的拖动留白，让底栏原有图标外侧边距等宽，不移动图标。
        SidebarGripColumn.Width = new GridLength(_isCompactLayout || !expanded ? 0 : ShellLayoutMetrics.SidebarGripGutter);
        // Span the root grid to keep the full eight-DIP drag target unclipped.
        // 拖动命中区跨根网格，保留完整 8 DIP 范围，避免窄列裁剪。
        SidebarGrip.Margin = new Thickness(preferredWidth - (SidebarGrip.Width - ShellLayoutMetrics.SidebarGripGutter) / 2, 0, 0, 0);
        SidebarPane.Width = _isCompactLayout && expanded ? preferredWidth : double.NaN;
        SidebarPane.HorizontalAlignment = _isCompactLayout && expanded ? HorizontalAlignment.Left : HorizontalAlignment.Stretch;
        Grid.SetColumnSpan(SidebarPane, _isCompactLayout && expanded ? 3 : 1);
        SidebarScrim.Visibility = _isCompactLayout && expanded ? Visibility.Visible : Visibility.Collapsed;
        RecentArea.Visibility = GlobalNavigationArea.Visibility = ChatWorkSwitcher.Visibility = expanded ? Visibility.Visible : Visibility.Collapsed;
        CompactSidebarButton.Visibility = expanded ? Visibility.Collapsed : Visibility.Visible;
        CollapseSidebarButton.Visibility = expanded ? Visibility.Visible : Visibility.Collapsed;
        SidebarGrip.Visibility = _isCompactLayout || !expanded ? Visibility.Collapsed : Visibility.Visible;
        ModelStatusButton.Visibility = expanded ? Visibility.Visible : Visibility.Collapsed;
        SidebarFooter.Margin = expanded ? new Thickness(14, 0, 10, 0) : new Thickness(8, 0, 8, 0);
    }

    private void SidebarScrim_Click(object sender, RoutedEventArgs e)
    {
        _temporarySidebarOpen = false;
        ApplyLayout();
        CompactSidebarButton.Focus(FocusState.Programmatic);
    }

    private void Presentation_KeyDown(object sender, KeyRoutedEventArgs args)
    {
        if (args.Key == VirtualKey.Escape && (TryDismissCompactSidebar() || TryDismissCompactWorkPanel())) args.Handled = true;
    }

    private bool TryDismissCompactSidebar()
    {
        if (!_temporarySidebarOpen || HasPresentationModal() || XamlRoot is null) return false;
        // Let native menus consume Escape before closing the drawer behind them.
        // 先让原生菜单处理 Escape，避免同时关闭其背后的抽屉。
        if (VisualTreeHelper.GetOpenPopupsForXamlRoot(XamlRoot).Any(popup => popup.IsOpen)) return false;
        SidebarScrim_Click(this, new RoutedEventArgs());
        return true;
    }

    private void ConnectionsNavigation_Click(object sender, RoutedEventArgs e) => OpenAgentTools();
    private void SkillsNavigation_Click(object sender, RoutedEventArgs e) => OpenAgentTools(skills: true);
    private void KnowledgeNavigation_Click(object sender, RoutedEventArgs e)
    {
        if (HasPresentationModal() || _chatClosing || !IsLoaded) return;
        OpenRetrievalSettings();
    }
    private async void ScheduleNavigation_Click(object sender, RoutedEventArgs e) =>
        await ShowFeatureInfoAsync("定时任务", "定时任务尚未开放。当前任务由你发送消息后启动，不会自动按计划运行。");

    private bool HasPresentationModal()
    {
        if (_presentationDialogOpen) return true;
        if (XamlRoot is null) return false;
        // Respect any dialog on this XamlRoot, including existing approvals and settings.
        // 尊重此 XamlRoot 上的所有弹窗，包括既有审批和设置。
        static bool ContainsDialog(DependencyObject? element)
        {
            if (element is ContentDialog) return true;
            if (element is null) return false;
            for (int i = 0; i < VisualTreeHelper.GetChildrenCount(element); i++)
                if (ContainsDialog(VisualTreeHelper.GetChild(element, i))) return true;
            return false;
        }
        return VisualTreeHelper.GetOpenPopupsForXamlRoot(XamlRoot).Any(popup => popup.IsOpen && ContainsDialog(popup.Child));
    }

    private async Task ShowFeatureInfoAsync(string title, string content)
    {
        if (HasPresentationModal() || _chatClosing || XamlRoot is null) return;
        _presentationDialogOpen = true;
        try
        {
            await DialogPresentation.ShowAsync(new ContentDialog
            {
                XamlRoot = XamlRoot, Title = UiText.Get(title), Content = UiText.Get(content),
                CloseButtonText = UiText.Get("知道了"), DefaultButton = ContentDialogButton.Close
            });
        }
        catch (Exception error) when (error is System.Runtime.InteropServices.COMException or InvalidOperationException)
        {
            ShowActionFeedback(UiText.Get("请先关闭当前弹窗，再打开此功能。"));
        }
        finally { _presentationDialogOpen = false; }
    }

    public void ShowNavigationMenu(FrameworkElement anchor)
    {
        if (HasPresentationModal() || _chatClosing) return;
        var menu = new MenuFlyout();
        void Add(string key, Action action, bool enabled = true)
        {
            var item = new MenuFlyoutItem { Text = UiText.Get(key), IsEnabled = enabled };
            item.Click += (_, _) => action();
            menu.Items.Add(item);
        }
        Add("新建聊天 Ctrl+N", StartNewChatFromNavigation, _projectsReady && !_projectActionPending && !_sendingPrompt);
        Add("查找会话正文 Ctrl+F", OpenTranscriptSearch);
        Add("搜索会话 Ctrl+Shift+K", () => ShowHistorySearch(anchor), _projectsReady);
        Add("聚焦输入 Ctrl+L", () => PromptTextBox.Focus(FocusState.Programmatic));
        Add("键盘操作", () => _ = ShowFeatureInfoAsync("键盘操作", "Enter 发送，Shift+Enter 换行；Ctrl+F 查找会话正文，Ctrl+Shift+K 搜索会话，Ctrl+L 聚焦输入，Ctrl+N 新建聊天，F1 查看帮助。查找框内 Enter／↓ 下一处、Shift+Enter／↑ 上一处；F3／Shift+F3 在任意区域切换，Esc 关闭。生成期间点击停止可取消当前回复。"));
        menu.Items.Add(new MenuFlyoutSeparator());
        Add("模型管理", () => RunAfterFlyoutClosed(menu, () => OpenModelManagement()));
        Add("工具与技能", () => RunAfterFlyoutClosed(menu, () => OpenAgentTools()));
        Add("设置", () => RunAfterFlyoutClosed(menu, () => StorageSettings_Click(anchor, new RoutedEventArgs())));
        Add("展开或收起侧栏", () => SidebarToggle_Click(anchor, new RoutedEventArgs()));
        menu.ShowAt(anchor);
    }

    private void RunAfterFlyoutClosed(FlyoutBase flyout, Action action)
    {
        void EnqueueAction() => DispatcherQueue.TryEnqueue(() =>
        {
            if (IsLoaded && !_chatClosing) action();
        });
        if (!flyout.IsOpen)
        {
            EnqueueAction();
            return;
        }
        // Wait for the flyout to unload and its close stack to unwind before activating another HWND.
        // Click 早于菜单卸载；卸载并退出关闭调用栈后再创建或激活窗口，避免焦点恢复冲突。
        void Closed(object? sender, object args)
        {
            flyout.Closed -= Closed;
            EnqueueAction();
        }
        flyout.Closed += Closed;
        flyout.Hide();
    }

    private void StartNewChatFromNavigation()
    {
        if (!_projectsReady || _projectActionPending || _sendingPrompt || _promptCompositionActive || HasPresentationModal()) return;
        if (ViewModel.IsChatMode) NewStandaloneChat_Click(this, new RoutedEventArgs());
        else if (_selectedWorkProjectId is Guid projectId && _projects.FirstOrDefault(p => p.Id == projectId) is { } project)
            StartNewProjectChat(project);
        else
        {
            SetPrimaryMode(true);
            NewStandaloneChat_Click(this, new RoutedEventArgs());
        }
    }

    private void SearchShortcut_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        if (_promptCompositionActive || HasPresentationModal()) return;
        OpenConversationSearch();
        args.Handled = true;
    }
    private void ComposerShortcut_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        if (_promptCompositionActive || HasPresentationModal()) return;
        PromptTextBox.Focus(FocusState.Programmatic);
        args.Handled = true;
    }
    private void NewChatShortcut_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        if (_promptCompositionActive || HasPresentationModal()) return;
        StartNewChatFromNavigation();
        args.Handled = true;
    }
    private async void HelpShortcut_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        if (_promptCompositionActive || HasPresentationModal()) return;
        args.Handled = true;
        await ShowFeatureInfoAsync("键盘操作", "Enter 发送，Shift+Enter 换行；Ctrl+F 查找会话正文，Ctrl+Shift+K 搜索会话，Ctrl+L 聚焦输入，Ctrl+N 新建聊天，F1 查看帮助。查找框内 Enter／↓ 下一处、Shift+Enter／↑ 上一处；F3／Shift+F3 在任意区域切换，Esc 关闭。生成期间点击停止可取消当前回复。");
    }

    private void ShowHistorySearch(FrameworkElement _) => OpenSidebarSearch(ViewModel.IsChatMode ? "chat" : "recent");

    private string CurrentConversationMarkdown()
    {
        var chat = ViewModel.IsChatMode ? _activeStandaloneChat : _activeProjectChat;
        return chat is null ? string.Empty : ConversationExport.ToMarkdown(chat.Title, chat.Messages.ToArray(), UiText.Get("用户"), "KYNXA");
    }

    private bool CopyConversationText(string text)
    {
        try
        {
            var package = new DataPackage { RequestedOperation = DataPackageOperation.Copy };
            package.SetText(text);
            Clipboard.SetContent(package);
            Clipboard.Flush();
            return true;
        }
        catch (Exception error) when (error is System.Runtime.InteropServices.COMException or ArgumentException or InvalidOperationException)
        {
            ShowActionFeedback(UiText.Get("复制失败，请重试。"));
            return false;
        }
    }

    private void ConversationActions_Click(object sender, RoutedEventArgs e)
    {
        var menu = new MenuFlyout();
        bool hasMessages = ActiveMessages.Count > 0;
        var copy = new MenuFlyoutItem { Text = UiText.Get("复制整个会话"), IsEnabled = hasMessages };
        copy.Click += (_, _) => { if (CopyConversationText(CurrentConversationMarkdown())) ShowActionFeedback(UiText.Get("已复制整个会话")); };
        var export = new MenuFlyoutItem { Text = UiText.Get("导出为 Markdown"), IsEnabled = hasMessages };
        export.Click += ExportConversation_Click;
        menu.Items.Add(copy); menu.Items.Add(export);
        menu.ShowAt((FrameworkElement)sender);
    }

    private async void ExportConversation_Click(object sender, RoutedEventArgs e)
    {
        var chat = ViewModel.IsChatMode ? _activeStandaloneChat : _activeProjectChat;
        if (chat is null || chat.Messages.Count == 0 || _chatClosing) return;
        // Capture the projection before the picker awaits; a later chat switch cannot export another chat.
        // 在文件选择器等待前捕获展示投影，随后切换聊天不会导出另一段会话。
        string content = CurrentConversationMarkdown();
        Guid conversationId = chat.Id;
        try
        {
            string title = new(chat.Title.Select(c => Path.GetInvalidFileNameChars().Contains(c) || char.IsControl(c) ? '_' : c).Take(72).ToArray());
            if (title.Length > 0 && char.IsHighSurrogate(title[^1])) title = title[..^1];
            var picker = new FileSavePicker
            {
                SuggestedStartLocation = PickerLocationId.DocumentsLibrary,
                SuggestedFileName = "KYNXA-" + title.Trim().TrimEnd('.')
            };
            picker.FileTypeChoices.Add("Markdown", [".md"]);
            WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(App.Window));
            var file = await picker.PickSaveFileAsync();
            if (file is null || _chatClosing) return;
            await FileIO.WriteTextAsync(file, content);
            if (!_chatClosing && ActiveChatId == conversationId) ShowActionFeedback(UiText.Get("会话已导出"));
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or System.Runtime.InteropServices.COMException or ArgumentException)
        {
            if (!_chatClosing) ShowActionFeedback(UiText.Get("无法导出会话，请确认文件位置可写后重试。"));
        }
    }
}
