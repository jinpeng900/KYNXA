using KYNXA_Desktop.ViewModels;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using System.Numerics;
using KYNXA_Desktop.Layout;
using static KYNXA_Desktop.Layout.ShellLayoutMetrics;

namespace KYNXA_Desktop.Views;

/// <summary>
/// The default KYNXA desktop shell defined by UI Design Spec v1.1.
/// 符合 UI Design Spec v1.1 的默认 KYNXA 桌面主界面。
/// </summary>
public sealed partial class ShellPage : Page
{
    private readonly LayoutStateService _layoutStateService = new();
    private LayoutState _layout = LayoutState.CreateDefault();
    private double _dragStartValue;
    private double _autoComposerHeight = ComposerHeightDefault;
    private bool _composerExpanded;
    private bool _applyingLayout;
    private string _workDraft = string.Empty;
    private string _chatDraft = string.Empty;
    private string _workConversationTitle = string.Empty;
    private string _chatConversationTitle = string.Empty;

    private Microsoft.UI.Xaml.Media.Animation.Storyboard? _sidebarTransition;
    private ModelManagementWindow? _modelManagementWindow;

    public ShellViewModel ViewModel { get; } = new();

    public ShellPage()
    {
        InitializeComponent();
        InitializePresentationActions();
        Unloaded += (_, _) => DetachLanguageUpdates();
        ConversationMessages.RetryRequested += Transcript_RetryRequested;
        ConversationMessages.ToolResultRequested += ToolResultRequested;
        ScreenshotPanel.ConfigureApi(_agentApiClient);
        ScreenshotPanel.ScreenshotOpenRequested += ToolResultRequested;
        ScreenshotPanel.ScreenshotsChanged += (_, _) => { if (!_chatClosing) ApplyLayout(); };
        InitializeWorkTabs();
        MountedWorkspace.OpenRequested += MountedWorkspaceOpenRequested;
        MountedWorkspace.ChangeRequested += MountedWorkspaceChangeRequested;
        MountedWorkspace.UnmountRequested += MountedWorkspaceUnmountRequested;
        // A click in native chrome/input is outside the browser document too.
        // 在原生界面区域或输入控件单击，也属于浏览器文档之外的操作。
        AddHandler(UIElement.PointerPressedEvent, new PointerEventHandler((_, e) =>
        {
            for (DependencyObject? node = e.OriginalSource as DependencyObject; node is not null; node = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetParent(node))
                if (node == ConversationMessages) return;
            ConversationMessages.ClearSelection();
        }), handledEventsToo: true);
        PromptTextBox.AddHandler(UIElement.KeyDownEvent,
            new KeyEventHandler(PromptTextBox_KeyDown), handledEventsToo: true);
        PromptTextBox.AddHandler(UIElement.KeyUpEvent,
            new KeyEventHandler(PromptTextBox_KeyUp), handledEventsToo: true);
    }

    private async void PageRoot_Loaded(object sender, RoutedEventArgs e)
    {
        AttachLanguageUpdates();
        MessageTimePresentation.ConfigureCache(StoragePaths.DesktopDirectory);
        _layout = _layoutStateService.Load();
        AppearanceService.Apply(_layout.AppearancePaletteId);
        UpdateWorkRecentVisibility();
        ConversationMessages.Preload();
        InitializeModelPicker();
        InitializePermissionPicker();
        // Present the current mode before awaiting the gateway; loading must not replace a user's later choice.
        // 等待网关前呈现当前模式；目录返回后不能覆盖用户在加载期间选择的模式和草稿。
        SetPrimaryMode(ViewModel.IsChatMode);
        await InitializeProjectsAsync();
        if (_projectViewClosed || !IsLoaded) return;
        UpdateWorkspacePickerVisibility();
        UpdateSendButtonState();
    }

    private void ApplyLayout()
    {
        if (_applyingLayout || WorkContextPanel is null) return;
        _applyingLayout = true;
        try
        {
            ApplySidebarPresentation();
            TopWorkspaceRow.Height = new GridLength(0);
            TopWorkspaceGrip.Visibility = Visibility.Collapsed;
            UpdateWorkspacePickerVisibility();

            double available = UpdateWorkPanelLayout();
            if (available > 0)
            {
                (double minimumComposerWidth, double maximumComposerWidth) = GetComposerWidthRange(available);
                ComposerHost.Width = Math.Clamp(_layout.ComposerWidth, minimumComposerWidth, maximumComposerWidth);
            }

            double maximumComposerHeight = GetComposerHeightMaximum();
            double minimumComposerHeight = Math.Min(ComposerHeightMin, maximumComposerHeight);
            double requestedComposerHeight = _composerExpanded
                ? maximumComposerHeight
                : Math.Max(_layout.ComposerHeight, _autoComposerHeight);
            ComposerHost.EditorHeight = Math.Clamp(requestedComposerHeight, minimumComposerHeight, maximumComposerHeight);
            // Measure the current localized actions instead of reserving a fixed label budget.
            // 测量当前语言下的操作区，避免为权限文字预留固定宽度。
            SelectedPermissionLabel.Visibility = ComposerHost.Width < 480 ? Visibility.Collapsed : Visibility.Visible;
            ComposerToolbarLeft.Measure(new Windows.Foundation.Size(double.PositiveInfinity, double.PositiveInfinity));
            double toolbarChrome = ComposerToolbar.Margin.Left + ComposerToolbar.Margin.Right
                + ComposerModelArea.Margin.Left + ComposerModelArea.Margin.Right
                + SendButton.Width + ComposerModelArea.ColumnSpacing
                + ModelPickerButton.Margin.Left + ModelPickerButton.Margin.Right;
            ModelPickerButton.MaxWidth = Math.Max(36, ComposerHost.Width - ComposerToolbarLeft.DesiredSize.Width - toolbarChrome);
            UpdateAdaptiveContentLayout(available);
        }
        finally { _applyingLayout = false; }
    }

    private void UpdateAdaptiveContentLayout(double availableWidth)
    {
        double width = availableWidth;
        double height = PrimaryContentSlot.ActualHeight;
        if (width <= 0 || height <= 0)
        {
            return;
        }

        // Scale gently around the reference layout while keeping compact windows usable.
        // 围绕参考布局缓慢缩放，同时保证小窗口仍可使用。
        double scale = Math.Clamp(Math.Min(width / 1100, height / 720), 0.76, 1.12);
        LogoHost.Width = 164 * scale;
        LogoHost.Height = 122 * scale;
        KynxaLogo.Width = 328 * scale;
        KynxaLogo.Height = 219 * scale;
        MainContentHost.Spacing = 12;
        LogoHost.Margin = new Thickness(0, 0, 0, Math.Clamp(18 + ((height - 520) * 0.03), 18, 32) - 12);

        double upwardOffset = Math.Clamp(height * 0.035, 12, 36);

        MainContentHost.Translation = ActiveMessages.Count > 0 ? Vector3.Zero : new Vector3(0, (float)-upwardOffset, 0);
        // The transcript fills the remaining grid column as the work panel resizes.
        // 工作面板调整宽度时，聊天填满网格中剩余的列空间。
        ConversationMessages.Margin = new Thickness(0, 54, 0, ComposerHost.SurfaceHeight + 64);

        AmbientLargeWave.Width = Math.Clamp(width * 0.72, 320, 1000);
        AmbientLargeWave.Height = Math.Clamp(height * 0.42, 180, 340);
        AmbientSoftWave.Width = AmbientLargeWave.Width * 0.82;
        AmbientSoftWave.Height = AmbientLargeWave.Height * 0.8;
    }

    private double GetComposerHeightMaximum() =>
        ShellLayoutMetrics.GetComposerHeightMaximum(PrimaryContentSlot.ActualHeight, ComposerHost.FooterHeight);

    private void SaveLayout() => _layoutStateService.Save(_layout);

    private void ShellGrid_SizeChanged(object sender, SizeChangedEventArgs e) => ApplyLayout();

    private void MainRegion_SizeChanged(object sender, SizeChangedEventArgs e) => ApplyLayout();

    private void PrimaryContentSlot_SizeChanged(object sender, SizeChangedEventArgs e) =>
        ApplyLayout();

    private void SidebarToggle_Click(object sender, RoutedEventArgs e)
    {
        FocusState focusState = (sender as Control)?.FocusState == FocusState.Keyboard
            ? FocusState.Keyboard : FocusState.Programmatic;
        if (_isCompactLayout)
        {
            _temporarySidebarOpen = !_temporarySidebarOpen;
        }
        else
        {
            _layout.SidebarCollapsed = !_layout.SidebarCollapsed;
        }
        ApplyLayout();
        // Keep keyboard focus on the visible counterpart after hiding the clicked button.
        // 隐藏被点击的按钮后，将键盘焦点交给当前可见的对应按钮。
        (CompactSidebarButton.Visibility == Visibility.Visible ? CompactSidebarButton : CollapseSidebarButton)
            .Focus(focusState);
        if (!_isCompactLayout) SaveLayout();
    }

    private void SidebarGrip_DragStarted(object? sender, EventArgs e) =>
        _dragStartValue = SidebarColumn.ActualWidth;

    private void SidebarGrip_DragDelta(object? sender, ResizeDeltaEventArgs e)
    {
        double requested = _dragStartValue + e.Delta;
        if (_layout.SidebarCollapsed)
        {
            if (requested <= 96)
            {
                return;
            }

            _layout.SidebarCollapsed = false;
            requested = SidebarMin;
        }

        if (requested <= 72)
        {
            _layout.SidebarCollapsed = true;
        }
        else
        {
            _layout.SidebarCollapsed = false;
            _layout.SidebarWidth = Math.Clamp(requested, SidebarMin, SidebarMax);
        }

        ApplyLayout();
    }

    private void SidebarGrip_CancelRequested(object? sender, EventArgs e)
    {
        _layout.SidebarCollapsed = _dragStartValue <= SidebarCollapsed;
        if (!_layout.SidebarCollapsed)
        {
            _layout.SidebarWidth = _dragStartValue;
        }
        ApplyLayout();
    }

    private void SidebarGrip_ResetRequested(object? sender, EventArgs e)
    {
        _layout.SidebarCollapsed = false;
        _layout.SidebarWidth = SidebarDefault;
        ApplyLayout();
        SaveLayout();
    }

    private void TopWorkspaceGrip_DragStarted(object? sender, EventArgs e) =>
        _dragStartValue = TopWorkspaceRow.ActualHeight;

    private void TopWorkspaceGrip_DragDelta(object? sender, ResizeDeltaEventArgs e)
    {
        _layout.TopWorkspaceHeight = Math.Clamp(_dragStartValue + e.Delta, TopMin, TopMax);
        ApplyLayout();
    }

    private void TopWorkspaceGrip_CancelRequested(object? sender, EventArgs e)
    {
        _layout.TopWorkspaceHeight = _dragStartValue;
        ApplyLayout();
    }

    private void TopWorkspaceGrip_ResetRequested(object? sender, EventArgs e)
    {
        _layout.TopWorkspaceHeight = TopDefault;
        ApplyLayout();
        SaveLayout();
    }

    private void ComposerWidthGrip_DragStarted(object? sender, EventArgs e) =>
        _dragStartValue = ComposerHost.ActualWidth;

    private void ComposerLeftGrip_DragDelta(object? sender, ResizeDeltaEventArgs e) =>
        SetComposerWidth(_dragStartValue - (e.Delta * 2));

    private void ComposerRightGrip_DragDelta(object? sender, ResizeDeltaEventArgs e) =>
        SetComposerWidth(_dragStartValue + (e.Delta * 2));

    private void SetComposerWidth(double requested)
    {
        (double min, double max) = GetComposerWidthRange(PrimaryContentSlot.ActualWidth);
        _layout.ComposerWidth = Math.Clamp(requested, min, max);
        ApplyLayout();
    }

    private void ComposerWidthGrip_CancelRequested(object? sender, EventArgs e)
    {
        _layout.ComposerWidth = _dragStartValue;
        ApplyLayout();
    }

    private void ComposerWidthGrip_ResetRequested(object? sender, EventArgs e)
    {
        _layout.ComposerWidth = ComposerWidthDefault;
        ApplyLayout();
        SaveLayout();
    }

    private void ComposerHeightGrip_DragStarted(object? sender, EventArgs e)
    {
        _composerExpanded = false;
        UpdateComposerExpandVisual();
        _dragStartValue = ComposerHost.EditorHeight;
    }

    private void ComposerTopGrip_DragDelta(object? sender, ResizeDeltaEventArgs e)
    {
        _layout.ComposerHeight = Math.Clamp(
            _dragStartValue - e.Delta,
            Math.Min(ComposerHeightMin, GetComposerHeightMaximum()),
            GetComposerHeightMaximum());
        ApplyLayout();
    }

    private void ComposerHeightGrip_CancelRequested(object? sender, EventArgs e)
    {
        _layout.ComposerHeight = _dragStartValue;
        ApplyLayout();
    }

    private void ComposerHeightGrip_ResetRequested(object? sender, EventArgs e)
    {
        _composerExpanded = false;
        UpdateComposerExpandVisual();
        _layout.ComposerHeight = ComposerHeightDefault;
        ApplyLayout();
        SaveLayout();
    }

    private void LayoutGrip_DragCompleted(object? sender, EventArgs e) => SaveLayout();

    private void ChatModeButton_Click(object sender, RoutedEventArgs e) => SetPrimaryMode(true);

    private void WorkModeButton_Click(object sender, RoutedEventArgs e) => SetPrimaryMode(false);

    private void SetPrimaryMode(bool chat, bool updateConversation = true)
    {
        bool changed = ViewModel.IsChatMode != chat;
        if (changed)
        {
            if (ViewModel.IsChatMode)
            {
                _chatDraft = PromptTextBox.Text;
                CaptureStandaloneDraft();
                DiscardEmptyStandaloneChats();
            }
            else
            {
                _workDraft = PromptTextBox.Text;
                CaptureProjectDraft();
                DiscardEmptyProjectChats();
            }
        }
        ViewModel.IsChatMode = chat;
        string primaryContent = chat ? "chat" : "work";
        bool preferenceChanged = _layout.LastPrimaryContent != primaryContent;
        _layout.LastPrimaryContent = primaryContent;
        UpdateModeSelection();
        WorkModeButton.FontWeight = chat ? Microsoft.UI.Text.FontWeights.Normal : Microsoft.UI.Text.FontWeights.Medium;
        ChatModeButton.FontWeight = chat ? Microsoft.UI.Text.FontWeights.Medium : Microsoft.UI.Text.FontWeights.Normal;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(WorkModeButton, chat ? UiText.Get("未选中") : UiText.Get("已选中"));
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(ChatModeButton, chat ? UiText.Get("已选中") : UiText.Get("未选中"));
        WorkSidebarContent.Visibility = chat ? Visibility.Collapsed : Visibility.Visible;
        ChatSidebarContent.Visibility = chat ? Visibility.Visible : Visibility.Collapsed;
        ChatAmbientLayer.Visibility = Visibility.Visible;
        MainContentHost.Visibility = Visibility.Visible;
        if (changed)
        {
            // TextBox's two-way source can still be pending a focus change.
            // TextBox 双向绑定的源值可能仍在等待焦点改变后提交。
            PromptTextBox.Text = chat ? _chatDraft : _workDraft;
            ViewModel.Prompt = PromptTextBox.Text;
            AnimateSidebarEntrance(chat ? ChatSidebarContent : WorkSidebarContent);
        }
        if (updateConversation)
        {
            UpdateConversationTitle();
            UpdateConversationPresentation();
        }
        PromptTextBox.PlaceholderText = chat ? UiText.Get("向 KYNXA 提问任何问题...") : UiText.Get("描述你想完成的工作...");
        if (changed || preferenceChanged) SaveLayout();
    }

    private void ModeSwitchTrack_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (PrimaryModeSelectionPill is null) return;
        // Resize immediately; only mode changes animate between the two equal halves.
        // 尺寸拖动立即生效；只有模式切换才在相等的左右半区间播放动画。
        var transition = PrimaryModeSelectionPill.TranslationTransition;
        PrimaryModeSelectionPill.TranslationTransition = null;
        UpdateModeSelection();
        PrimaryModeSelectionPill.TranslationTransition = transition;
    }

    private void UpdateModeSelection()
    {
        double half = ModeSwitchTrack.ActualWidth / 2;
        if (half <= 0) return;
        PrimaryModeSelectionPill.Width = half;
        PrimaryModeSelectionPill.Translation = new Vector3(ViewModel.IsChatMode ? (float)half : 0, 0, 0);
    }

    private void AnimateSidebarEntrance(FrameworkElement target)
    {
        _sidebarTransition?.Stop();
        var fade = new Microsoft.UI.Xaml.Media.Animation.DoubleAnimation
        {
            From = 0.5, To = 1,
            Duration = new Duration(TimeSpan.FromMilliseconds(120)),
            FillBehavior = Microsoft.UI.Xaml.Media.Animation.FillBehavior.Stop
        };
        Microsoft.UI.Xaml.Media.Animation.Storyboard.SetTarget(fade, target);
        Microsoft.UI.Xaml.Media.Animation.Storyboard.SetTargetProperty(fade, "Opacity");
        _sidebarTransition = new Microsoft.UI.Xaml.Media.Animation.Storyboard();
        _sidebarTransition.Children.Add(fade);
        _sidebarTransition.Begin();
    }

    private void UpdateConversationTitle()
    {
        ConversationTitle.Text = ViewModel.IsChatMode ? _chatConversationTitle : _workConversationTitle;
        ConversationTitle.Visibility = string.IsNullOrEmpty(ConversationTitle.Text) ? Visibility.Collapsed : Visibility.Visible;
    }

    private void ProjectsToggle_Click(object sender, RoutedEventArgs e)
    {
        bool visible = ProjectTree.Visibility == Visibility.Visible;
        ProjectTree.Visibility = visible ? Visibility.Collapsed : Visibility.Visible;
        ProjectsChevron.Glyph = visible ? "\uE76C" : "\uE70D";
        UpdateWorkSidebarHeights(WorkSidebarContent.ActualHeight);
    }

    private void WorkSidebarContent_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (WorkRecentHistory is null || ProjectTree is null) return;
        // The task list gets all remaining height, above the fixed sidebar footer.
        // 任务列表占用固定侧栏页脚上方的全部剩余高度。
        UpdateWorkSidebarHeights(e.NewSize.Height);
    }

    private void ChatHistoryList_ItemClick(object sender, ItemClickEventArgs e)
    {
        if (e.ClickedItem is RecentConversation conversation) SelectStandaloneChat(conversation);
    }
    private void ConversationRow_PointerEntered(object sender, PointerRoutedEventArgs e)
    {
        if (sender is not FrameworkElement row)
        {
            return;
        }

        Button? conversationButton = row.FindName("ConversationOpenButton") as Button ??
            FindTaggedButton(row, "ConversationOpen");
        if (conversationButton is not null)
        {
            conversationButton.Background =
                (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["KynxaControlHoverBrush"];
        }

        Button? moreButton = row.FindName("ConversationMoreButton") as Button ??
            FindTaggedButton(row, "ConversationMore");
        if (moreButton is not null)
        {
            moreButton.IsHitTestVisible = true;
            moreButton.Opacity = 1;
        }
    }

    private void ConversationRow_PointerExited(object sender, PointerRoutedEventArgs e)
    {
        if (sender is not FrameworkElement row)
        {
            return;
        }

        Button? conversationButton = row.FindName("ConversationOpenButton") as Button ??
            FindTaggedButton(row, "ConversationOpen");
        if (conversationButton is not null)
        {
            conversationButton.Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(Microsoft.UI.Colors.Transparent);
        }

        Button? moreButton = row.FindName("ConversationMoreButton") as Button ??
            FindTaggedButton(row, "ConversationMore");
        if (moreButton is not null)
        {
            moreButton.Opacity = 0;
            moreButton.IsHitTestVisible = false;
        }
    }

    private static Button? FindTaggedButton(FrameworkElement row, string tag)
    {
        if (row is not Panel panel)
        {
            return null;
        }

        foreach (UIElement child in panel.Children)
        {
            if (child is Button { Tag: string buttonTag } button && buttonTag == tag)
            {
                return button;
            }
        }

        return null;
    }

    private void ModelManagementButton_Click(object sender, RoutedEventArgs e) => OpenModelManagement();

    private void OpenModelManagement(bool customModels = false)
    {
        if (_modelManagementWindow is not null)
        {
            if (customModels) _modelManagementWindow.ShowCustomModels();
            _modelManagementWindow.Activate();
            return;
        }

        _modelManagementWindow = new ModelManagementWindow();
        _modelManagementWindow.Closed += ModelManagementWindow_Closed;
        if (customModels) _modelManagementWindow.ShowCustomModels();
        _modelManagementWindow.Activate();
    }

    private void ModelManagementWindow_Closed(object sender, WindowEventArgs args)
    {
        _modelManagementWindow = null;
        _ = RefreshModelPickerAsync();
    }

    private void PromptTextBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        UpdateSendButtonState();
        // Match the editor's reserved scrollbar lane so wrapping never runs beneath it.
        // 与输入区预留的滚动条通道对齐，避免换行文本延伸到滚动条下。
        double availableTextWidth = Math.Max(120, PromptTextBox.ActualWidth - 88);
        TextBlock measure = new()
        {
            Text = string.IsNullOrEmpty(PromptTextBox.Text) ? " " : PromptTextBox.Text,
            FontFamily = PromptTextBox.FontFamily,
            FontSize = PromptTextBox.FontSize,
            FontStyle = PromptTextBox.FontStyle,
            FontWeight = PromptTextBox.FontWeight,
            TextWrapping = TextWrapping.Wrap,
            Width = availableTextWidth
        };
        measure.Measure(new Windows.Foundation.Size(availableTextWidth, double.PositiveInfinity));

        double extraContentHeight = Math.Max(0, measure.DesiredSize.Height - 24);
        double maximumComposerHeight = GetComposerHeightMaximum();
        double minimumAutoHeight = Math.Min(ComposerHeightDefault, maximumComposerHeight);
        double maximumAutoHeight = Math.Max(
            minimumAutoHeight,
            Math.Min(ComposerAutoHeightMax, maximumComposerHeight));
        _autoComposerHeight = Math.Clamp(
            ComposerHeightDefault + extraContentHeight,
            minimumAutoHeight,
            maximumAutoHeight);
        ApplyLayout();
    }

    private void ComposerExpandButton_Click(object sender, RoutedEventArgs e)
    {
        _composerExpanded = !_composerExpanded;
        UpdateComposerExpandVisual();
        ApplyLayout();
        PromptTextBox.Focus(FocusState.Programmatic);
    }

    private void UpdateComposerExpandVisual()
    {
        ComposerExpandIcon.Glyph = _composerExpanded ? "\uE73F" : "\uE740";
        ToolTipService.SetToolTip(
            ComposerExpandButton,
            _composerExpanded ? UiText.Get("收起输入区") : UiText.Get("展开输入区"));
    }

    private void NewPrimaryAction_Click(object sender, RoutedEventArgs e)
    {
        if (!ViewModel.IsChatMode)
        {
            return;
        }

        ViewModel.Prompt = string.Empty;
        PromptTextBox.Focus(FocusState.Programmatic);
    }

}
