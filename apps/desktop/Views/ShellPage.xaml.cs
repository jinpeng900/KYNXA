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

/// <summary>The default KYNXA desktop shell defined by UI Design Spec v1.1.</summary>
public sealed partial class ShellPage : Page
{
    private readonly LayoutStateService _layoutStateService = new();
    private LayoutState _layout = LayoutState.CreateDefault();
    private double _dragStartValue;
    private double _autoComposerHeight = ComposerHeightDefault;
    private bool _composerExpanded;
    private bool _applyingLayout;
    private bool _compactSidebarOpen;
    private Guid? _presentedChatId;
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
        KeyDown += ShellPage_KeyDown;
        PromptTextBox.AddHandler(UIElement.KeyDownEvent,
            new KeyEventHandler(PromptTextBox_KeyDown), handledEventsToo: true);
        PromptTextBox.AddHandler(UIElement.KeyUpEvent,
            new KeyEventHandler(PromptTextBox_KeyUp), handledEventsToo: true);
    }

    private async void PageRoot_Loaded(object sender, RoutedEventArgs e)
    {
        _layout = _layoutStateService.Load();
        ApplyLayout();
        InitializeModelPicker();
        InitializePermissionPicker();
        await InitializeProjectsAsync();
        SetPrimaryMode(false);
    }

    private void ApplyLayout()
    {
        if (_applyingLayout || WorkContextPanel is null) return;
        _applyingLayout = true;
        try
        {
            bool narrowWindow = ShellGrid.ActualWidth is > 0 and < 960;
            if (!narrowWindow) _compactSidebarOpen = false;
            bool overlaySidebar = narrowWindow && _compactSidebarOpen;
            bool compactSidebar = !overlaySidebar && (_layout.SidebarCollapsed || narrowWindow);
            double sidebar = compactSidebar
                ? SidebarCollapsed
                : Math.Clamp(_layout.SidebarWidth, SidebarMin, SidebarMax);

            SidebarColumn.Width = new GridLength(narrowWindow ? SidebarCollapsed : sidebar);
            Grid.SetColumnSpan(SidebarSurface, overlaySidebar ? 3 : 1);
            Canvas.SetZIndex(SidebarSurface, overlaySidebar ? 20 : 0);
            SidebarSurface.Width = overlaySidebar ? Math.Min(280, Math.Max(56, ShellGrid.ActualWidth - 48)) : double.NaN;
            SidebarSurface.HorizontalAlignment = overlaySidebar ? HorizontalAlignment.Left : HorizontalAlignment.Stretch;
            SidebarDismissLayer.Visibility = overlaySidebar ? Visibility.Visible : Visibility.Collapsed;
            SidebarGrip.Visibility = narrowWindow ? Visibility.Collapsed : Visibility.Visible;
            RecentArea.Visibility = compactSidebar ? Visibility.Collapsed : Visibility.Visible;
            ChatWorkSwitcher.Visibility = compactSidebar ? Visibility.Collapsed : Visibility.Visible;
            CompactSidebarButton.Visibility = compactSidebar ? Visibility.Visible : Visibility.Collapsed;
            GlobalNavigationArea.Visibility = compactSidebar ? Visibility.Collapsed : Visibility.Visible;
            SidebarNavigationScroll.Visibility = compactSidebar ? Visibility.Collapsed : Visibility.Visible;
            SidebarNavigationScroll.MaxHeight = Math.Max(80, ShellGrid.ActualHeight - 264);
            GatewayStatusHost.Visibility = compactSidebar ? Visibility.Collapsed : Visibility.Visible;
            SidebarStatusColumn.Width = new GridLength(compactSidebar ? 0 : 32);
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
            SelectedPermissionLabel.Visibility = ComposerHost.Width < 520 ? Visibility.Collapsed : Visibility.Visible;
            ModelPickerButton.MaxWidth = Math.Max(48, ComposerHost.Width - (ComposerHost.Width < 520 ? 154 : 250));
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
        bool hasMessages = ActiveMessages.Count > 0;
        LogoHost.Visibility = !hasMessages && height >= 560 ? Visibility.Visible : Visibility.Collapsed;
        HomeIntro.Visibility = hasMessages ? Visibility.Collapsed : Visibility.Visible;
        HomeActions.Visibility = height >= 360 ? Visibility.Visible : Visibility.Collapsed;
        HomeDescription.Visibility = height >= 320 ? Visibility.Visible : Visibility.Collapsed;
        HomeExamples.Visibility = !hasMessages && height >= 460 && width >= 440 ? Visibility.Visible : Visibility.Collapsed;
        HomeIntro.Width = Math.Max(0, ComposerHost.Width - 8);
        ModelStatusBar.Width = ComposerHost.Width;
        LogoHost.Width = 112;
        LogoHost.Height = 76;
        KynxaLogo.Width = 168;
        KynxaLogo.Height = 112;
        MainContentHost.Spacing = hasMessages ? 6 : height >= 560 ? 18 : 10;

        double upwardOffset = height >= 560 ? 12 : 0;

        MainContentHost.Translation = ActiveMessages.Count > 0 ? Vector3.Zero : new Vector3(0, (float)-upwardOffset, 0);
        ConversationMessages.Width = Math.Min(ConversationWidthMax, Math.Max(0, width - 48));
        ConversationMessages.Margin = new Thickness(0, 54, 0, ComposerHost.SurfaceHeight +
            (ModelStatusBar.Visibility == Visibility.Visible ? ModelStatusBar.ActualHeight + 12 : 0) + 32);
        JumpToLatestButton.Margin = new Thickness(0, 0, 0, ConversationMessages.Margin.Bottom + 6);

        AmbientLargeWave.Width = Math.Clamp(width * 0.72, 320, 1000);
        AmbientLargeWave.Height = Math.Clamp(height * 0.42, 180, 340);
        AmbientSoftWave.Width = AmbientLargeWave.Width * 0.82;
        AmbientSoftWave.Height = AmbientLargeWave.Height * 0.8;
    }

    private double GetComposerHeightMaximum()
    {
        double height = PrimaryContentSlot.ActualHeight;
        if (height <= 0) return ComposerHeightMax;
        double reserve = ActiveMessages.Count > 0 ? 64 : height >= 560 ? 320 : height >= 460 ? 210 : height >= 360 ? 160 : 98;
        // MainWindow supplies a minimum viewport; never shrink the editor below one text line plus its tool row.
        return Math.Max(ComposerHeightMin, height <= reserve ? 0 :
            ShellLayoutMetrics.GetComposerHeightMaximum(height - reserve, ComposerHost.FooterHeight));
    }

    private void SaveLayout() => _layoutStateService.Save(_layout);

    private void ShellGrid_SizeChanged(object sender, SizeChangedEventArgs e) => ApplyLayout();

    private void MainRegion_SizeChanged(object sender, SizeChangedEventArgs e) => ApplyLayout();

    private void PrimaryContentSlot_SizeChanged(object sender, SizeChangedEventArgs e) =>
        ApplyLayout();

    private void SidebarToggle_Click(object sender, RoutedEventArgs e)
    {
        if (ShellGrid.ActualWidth is > 0 and < 960)
        {
            _compactSidebarOpen = !_compactSidebarOpen;
            ApplyLayout();
            return;
        }
        _layout.SidebarCollapsed = !_layout.SidebarCollapsed;
        ApplyLayout();
        SaveLayout();
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

    private void SetPrimaryMode(bool chat)
    {
        _compactSidebarOpen = false;
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
        SynchronizeHistorySearchMode();
        _layout.LastPrimaryContent = chat ? "chat" : "work";
        UpdateModeSelection();
        WorkModeButton.FontWeight = chat ? Microsoft.UI.Text.FontWeights.Normal : Microsoft.UI.Text.FontWeights.Medium;
        ChatModeButton.FontWeight = chat ? Microsoft.UI.Text.FontWeights.Medium : Microsoft.UI.Text.FontWeights.Normal;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(WorkModeButton, chat ? "未选中" : "已选中");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(ChatModeButton, chat ? "已选中" : "未选中");
        WorkSidebarScrollViewer.Visibility = chat ? Visibility.Collapsed : Visibility.Visible;
        ChatSidebarContent.Visibility = chat ? Visibility.Visible : Visibility.Collapsed;
        ChatAmbientLayer.Visibility = Visibility.Visible;
        MainContentHost.Visibility = Visibility.Visible;
        if (changed)
        {
            // TextBox's two-way source can still be pending a focus change.
            PromptTextBox.Text = chat ? _chatDraft : _workDraft;
            ViewModel.Prompt = PromptTextBox.Text;
            AnimateSidebarEntrance(chat ? ChatSidebarContent : WorkSidebarContent);
        }
        UpdateConversationTitle();
        UpdateConversationPresentation();
        PromptTextBox.PlaceholderText = chat ? "向 KYNXA 提问任何问题..." : "描述你想完成的工作...";
        SaveLayout();
    }

    private void ModeSwitchTrack_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (PrimaryModeSelectionPill is null) return;
        // Resize immediately; only mode changes animate between the two equal halves.
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
    }

    private void WorkSidebarContent_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (ProjectTree is null) return;
        // Use the bounded viewport; scrollable content measures itself with unlimited height.
        UpdateWorkSidebarHeights(WorkSidebarScrollViewer.ActualHeight);
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
        // Match the editor's reserved scrollbar lane so wrapping never runs beneath it.
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
            _composerExpanded ? "收起输入区" : "展开输入区");
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
