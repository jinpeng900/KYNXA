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
using Microsoft.UI.Xaml.Markup;
using Microsoft.UI.Xaml.Media;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;
using Windows.Storage.Pickers;
using Windows.System;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private DispatcherTimer? _actionFeedbackTimer;
    private Flyout? _historySearchFlyout;
    private bool _presentationDialogOpen;
    private bool _isCompactLayout;
    private bool _temporarySidebarOpen;
    private bool _presentationActionsAttached;
    private sealed record HistorySearchRow(Guid Id, Guid? ProjectId, string Title, string Scope);

    private void InitializePresentationActions()
    {
        AutomationProperties.SetLiveSetting(ActionFeedbackText, Microsoft.UI.Xaml.Automation.Peers.AutomationLiveSetting.Polite);
        // Register SDK key enums in code so the XAML compiler need not inspect their metadata.
        // 在代码中注册 SDK 按键枚举，避免 XAML 编译器解析其元数据。
        var search = new KeyboardAccelerator { Key = VirtualKey.F, Modifiers = VirtualKeyModifiers.Control };
        search.Invoked += SearchShortcut_Invoked;
        var composer = new KeyboardAccelerator { Key = VirtualKey.L, Modifiers = VirtualKeyModifiers.Control };
        composer.Invoked += ComposerShortcut_Invoked;
        var newChat = new KeyboardAccelerator { Key = VirtualKey.N, Modifiers = VirtualKeyModifiers.Control };
        newChat.Invoked += NewChatShortcut_Invoked;
        var help = new KeyboardAccelerator { Key = VirtualKey.F1 };
        help.Invoked += HelpShortcut_Invoked;
        KeyboardAccelerators.Add(search);
        KeyboardAccelerators.Add(composer);
        KeyboardAccelerators.Add(newChat);
        KeyboardAccelerators.Add(help);
        KeyDown += Presentation_KeyDown;
        Loaded += (_, _) =>
        {
            if (_presentationActionsAttached) return;
            ConversationMessages.ActionFeedbackRequested += Transcript_ActionFeedbackRequested;
            _presentationActionsAttached = true;
        };
        Unloaded += (_, _) =>
        {
            ConversationMessages.ActionFeedbackRequested -= Transcript_ActionFeedbackRequested;
            _presentationActionsAttached = false;
            _historySearchFlyout?.Hide();
            ClearActionFeedback();
        };
    }

    private void Transcript_ActionFeedbackRequested(object? sender, string text) => ShowActionFeedback(text);

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
        SidebarPane.Width = _isCompactLayout && expanded ? preferredWidth : double.NaN;
        SidebarPane.HorizontalAlignment = _isCompactLayout && expanded ? HorizontalAlignment.Left : HorizontalAlignment.Stretch;
        Grid.SetColumnSpan(SidebarPane, _isCompactLayout && expanded ? 3 : 1);
        SidebarScrim.Visibility = _isCompactLayout && expanded ? Visibility.Visible : Visibility.Collapsed;
        RecentArea.Visibility = GlobalNavigationArea.Visibility = ChatWorkSwitcher.Visibility = expanded ? Visibility.Visible : Visibility.Collapsed;
        CompactSidebarButton.Visibility = expanded ? Visibility.Collapsed : Visibility.Visible;
        SidebarGrip.Visibility = _isCompactLayout ? Visibility.Collapsed : Visibility.Visible;
        HistorySearchButton.Visibility = ModelStatusButton.Visibility = expanded ? Visibility.Visible : Visibility.Collapsed;
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
        if (args.Key == VirtualKey.Escape && TryDismissCompactSidebar()) args.Handled = true;
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
            await new ContentDialog
            {
                XamlRoot = XamlRoot, Title = UiText.Get(title), Content = UiText.Get(content),
                CloseButtonText = UiText.Get("知道了"), DefaultButton = ContentDialogButton.Close
            }.ShowAsync();
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
        Add("搜索会话 Ctrl+F", () => ShowHistorySearch(anchor), _projectsReady);
        Add("聚焦输入 Ctrl+L", () => PromptTextBox.Focus(FocusState.Programmatic));
        Add("键盘操作", () => _ = ShowFeatureInfoAsync("键盘操作", "Enter 发送，Shift+Enter 换行；Ctrl+F 搜索会话，Ctrl+L 聚焦输入，Ctrl+N 新建聊天，F1 查看帮助。生成期间点击停止可取消当前回复。"));
        menu.Items.Add(new MenuFlyoutSeparator());
        Add("模型管理", () => OpenModelManagement());
        Add("工具与技能", () => OpenAgentTools());
        Add("设置", () => StorageSettings_Click(anchor, new RoutedEventArgs()));
        Add("展开或收起侧栏", () => SidebarToggle_Click(anchor, new RoutedEventArgs()));
        menu.ShowAt(anchor);
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
        ShowHistorySearch(HistorySearchButton.Visibility == Visibility.Visible ? HistorySearchButton : CompactSidebarButton);
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
        await ShowFeatureInfoAsync("键盘操作", "Enter 发送，Shift+Enter 换行；Ctrl+F 搜索会话，Ctrl+L 聚焦输入，Ctrl+N 新建聊天，F1 查看帮助。生成期间点击停止可取消当前回复。");
    }

    private void HistorySearch_Click(object sender, RoutedEventArgs e) => ShowHistorySearch((FrameworkElement)sender);

    private void ShowHistorySearch(FrameworkElement anchor)
    {
        if (!_projectsReady || _chatClosing || XamlRoot is null || HasPresentationModal()) return;
        _historySearchFlyout?.Hide();
        var flyout = PickerMenu.Create(FlyoutPlacementMode.RightEdgeAlignedTop);
        _historySearchFlyout = flyout;
        var rows = _standaloneChats.Where(chat => !chat.IsArchived).OrderByDescending(chat => chat.IsPinned)
            .Select(chat => new HistorySearchRow(chat.Id, null, chat.Title, UiText.Get("聊天")))
            .Concat(_projects.Where(project => !project.IsArchived).SelectMany(project => project.Chats
                .Where(chat => !chat.IsArchived).OrderByDescending(chat => chat.IsPinned)
                .Select(chat => new HistorySearchRow(chat.Id, project.Id, chat.Title, project.Name)))).ToArray();
        var search = new TextBox { PlaceholderText = UiText.Get("搜索会话标题或项目"), Margin = new Thickness(4, 4, 4, 8) };
        AutomationProperties.SetName(search, UiText.Get("搜索会话"));
        AutomationProperties.SetAutomationId(search, "HistorySearchBox");
        var results = PickerMenu.CreateList("HistorySearchResults", UiText.Get("搜索结果"));
        // History rows have title and scope lines; the shared single-line picker height would clip the scope.
        // 会话结果包含标题和归属两行；共用单行选择器的固定高度会截断归属信息。
        var rowStyle = new Style(typeof(ListViewItem))
        {
            BasedOn = (Style)Application.Current.Resources["KynxaModelListItemStyle"]
        };
        rowStyle.Setters.Add(new Setter(FrameworkElement.HeightProperty, double.NaN));
        rowStyle.Setters.Add(new Setter(FrameworkElement.MinHeightProperty, 54d));
        rowStyle.Setters.Add(new Setter(Control.PaddingProperty, new Thickness(12, 4, 16, 4)));
        results.ItemContainerStyle = rowStyle;
        results.ItemTemplate = (DataTemplate)XamlReader.Load("""
            <DataTemplate xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation">
                <StackPanel Spacing="3" Padding="0,5">
                    <TextBlock Text="{Binding Title}" ToolTipService.ToolTip="{Binding Title}" FontSize="13" TextTrimming="CharacterEllipsis" />
                    <TextBlock Text="{Binding Scope}" ToolTipService.ToolTip="{Binding Scope}" FontSize="11" Foreground="#777777" TextTrimming="CharacterEllipsis" />
                </StackPanel>
            </DataTemplate>
            """);
        var empty = new TextBlock { Margin = new Thickness(10, 12, 10, 0), TextWrapping = TextWrapping.Wrap, FontSize = 13 };
        void Filter()
        {
            string query = search.Text.Trim();
            var matches = rows.Where(row => row.Title.Contains(query, StringComparison.OrdinalIgnoreCase)
                || row.Scope.Contains(query, StringComparison.OrdinalIgnoreCase)).ToArray();
            results.ItemsSource = matches;
            results.SelectedItem = matches.FirstOrDefault(row => row.Id == ActiveChatId) ?? matches.FirstOrDefault();
            empty.Text = UiText.Get(rows.Length == 0 ? "还没有会话，发送第一条消息后可在这里查找。" : "没有匹配的会话，请换个关键词。");
            empty.Visibility = matches.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
        }
        void Select(HistorySearchRow row)
        {
            if (_chatClosing || _projectActionPending || _sendingPrompt) return;
            // Revalidate stable identities against live data; search never rewrites the catalog.
            // 对照当前数据重新核验稳定身份；搜索从不改写目录。
            if (row.ProjectId is Guid projectId && _projects.FirstOrDefault(p => p.Id == projectId && !p.IsArchived) is { } project
                && project.Chats.FirstOrDefault(chat => chat.Id == row.Id && !chat.IsArchived) is { } workChat)
                SelectProjectChat(project, workChat);
            else if (row.ProjectId is null && _standaloneChats.Any(chat => chat.Id == row.Id && !chat.IsArchived))
            {
                SetPrimaryMode(true);
                SelectStandaloneChat(new RecentConversation(row.Title, string.Empty) { Id = row.Id });
                RebuildStandaloneRows();
            }
            else return;
            flyout.Hide();
            _temporarySidebarOpen = false;
            ApplyLayout();
        }
        search.TextChanged += (_, _) => Filter();
        results.ItemClick += (_, args) => { if (args.ClickedItem is HistorySearchRow row) Select(row); };
        bool composing = false, suppressEnter = false;
        search.TextCompositionStarted += (_, _) => composing = true;
        search.TextCompositionEnded += (_, _) => { composing = false; suppressEnter = true; };
        search.KeyUp += (_, _) => { if (!composing) suppressEnter = false; };
        search.KeyDown += (_, args) =>
        {
            if (args.Key == VirtualKey.Down && results.Items.Count > 0) { results.Focus(FocusState.Keyboard); args.Handled = true; }
            else if (args.Key == VirtualKey.Enter && !composing && !suppressEnter && results.SelectedItem is HistorySearchRow row)
            { Select(row); args.Handled = true; }
        };
        results.KeyDown += (_, args) =>
        {
            if (args.Key == VirtualKey.Enter && results.SelectedItem is HistorySearchRow row) { Select(row); args.Handled = true; }
        };
        var body = new Grid();
        body.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        body.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        body.Children.Add(search);
        Grid.SetRow(results, 1); Grid.SetRow(empty, 1);
        body.Children.Add(results); body.Children.Add(empty);
        body.Width = PickerMenu.SetContentWidth(flyout, 380, XamlRoot.Size.Width);
        search.Measure(new Windows.Foundation.Size(body.Width, double.PositiveInfinity));
        // Size the first result set once; typing does not move or shrink the open search popup.
        // 仅按首次结果数量确定高度；输入筛选条件不会移动或缩小已打开的搜索菜单。
        double initialResultsHeight = rows.Length == 0 ? 64 : rows.Length * 54d;
        body.Height = Math.Min(Math.Min(420, Math.Max(120, XamlRoot.Size.Height - 64)),
            search.DesiredSize.Height + initialResultsHeight);
        Filter();
        flyout.Content = body;
        flyout.Opened += (_, _) => search.Focus(FocusState.Programmatic);
        flyout.Closed += (_, _) => { if (ReferenceEquals(_historySearchFlyout, flyout)) _historySearchFlyout = null; };
        flyout.ShowAt(anchor);
    }

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
