using KYNXA_Desktop.ViewModels;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using System.Numerics;

namespace KYNXA_Desktop.Views;

/// <summary>The default KYNXA desktop shell defined by UI Design Spec v1.1.</summary>
public sealed partial class ShellPage : Page
{
    private const double SidebarDefault = 176;
    private const double SidebarCollapsed = 56;
    private const double SidebarMin = 144;
    private const double SidebarMax = 360;
    private const double TopDefault = 96;
    private const double TopMin = 72;
    private const double TopMax = 180;
    private const double ComposerWidthDefault = 824;
    private const double ComposerWidthMin = 620;
    private const double ComposerWidthMax = 1040;
    private const double ComposerHeightDefault = 110;
    private const double ComposerHeightMin = 96;
    private const double ComposerAutoHeightMax = 210;
    private const double ComposerHeightMax = 420;

    private readonly LayoutStateService _layoutStateService = new();
    private LayoutState _layout = LayoutState.CreateDefault();
    private double _dragStartValue;
    private double _autoComposerHeight = ComposerHeightDefault;
    private bool _composerExpanded;
    private bool _isWorkDetailMode;
    private WorkCategory? _selectedWorkCategory;

    public ShellViewModel ViewModel { get; } = new();

    public ShellPage()
    {
        InitializeComponent();
    }

    private void PageRoot_Loaded(object sender, RoutedEventArgs e)
    {
        _layout = _layoutStateService.Load();
        ApplyLayout();
        SetPrimaryMode(true);
    }

    private void ApplyLayout()
    {
        double sidebar = _layout.SidebarCollapsed
            ? SidebarCollapsed
            : Math.Clamp(_layout.SidebarWidth, SidebarMin, SidebarMax);

        SidebarColumn.Width = new GridLength(sidebar);
        WorkNavigationContent.Margin = _layout.SidebarCollapsed
            ? new Thickness(0)
            : new Thickness(0, 0, 26, 0);
        if (_layout.SidebarCollapsed)
        {
            WorkAddButton.Opacity = 0;
            WorkAddButton.IsHitTestVisible = false;
        }
        RecentArea.Visibility = _layout.SidebarCollapsed || !ViewModel.IsChatMode || _isWorkDetailMode
            ? Visibility.Collapsed
            : Visibility.Visible;
        TopWorkspaceRow.Height = _isWorkDetailMode
            ? new GridLength(0)
            : new GridLength(Math.Clamp(_layout.TopWorkspaceHeight, TopMin, TopMax));
        TopWorkspaceGrip.Visibility = _isWorkDetailMode ? Visibility.Collapsed : Visibility.Visible;

        double available = PrimaryContentSlot.ActualWidth > 0 ? PrimaryContentSlot.ActualWidth : MainRegion.ActualWidth;
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
        ComposerHost.Height = Math.Clamp(requestedComposerHeight, minimumComposerHeight, maximumComposerHeight);
        UpdateAdaptiveContentLayout();
    }

    private static (double Minimum, double Maximum) GetComposerWidthRange(double availableWidth)
    {
        double horizontalPadding = availableWidth switch
        {
            >= 1200 => 144,
            >= 800 => 96,
            >= 520 => 56,
            _ => 24
        };

        double maximum = Math.Min(ComposerWidthMax, Math.Max(240, availableWidth - horizontalPadding));
        return (Math.Min(ComposerWidthMin, maximum), maximum);
    }

    private void UpdateAdaptiveContentLayout()
    {
        double width = PrimaryContentSlot.ActualWidth;
        double height = PrimaryContentSlot.ActualHeight;
        if (width <= 0 || height <= 0)
        {
            return;
        }

        // Scale gently around the reference layout while keeping compact windows usable.
        double scale = Math.Clamp(Math.Min(width / 1100, height / 720), 0.76, 1.12);
        LogoHost.Width = 176 * scale;
        LogoHost.Height = 132 * scale;
        KynxaLogo.Width = 353 * scale;
        KynxaLogo.Height = 235 * scale;
        MainContentHost.Spacing = Math.Clamp(22 + ((height - 520) * 0.04), 22, 42);

        double upwardOffset = Math.Clamp(height * 0.035, 12, 36);
        ChatWorkSwitcher.Translation = new Vector3(0, 0, 16);
        MainContentHost.Translation = new Vector3(0, (float)-upwardOffset, 0);

        AmbientLargeWave.Width = Math.Clamp(width * 0.72, 320, 1000);
        AmbientLargeWave.Height = Math.Clamp(height * 0.42, 180, 340);
        AmbientSoftWave.Width = AmbientLargeWave.Width * 0.82;
        AmbientSoftWave.Height = AmbientLargeWave.Height * 0.8;
    }

    private double GetComposerHeightMaximum()
    {
        double availableHeight = PrimaryContentSlot.ActualHeight;
        if (availableHeight <= 0)
        {
            return ComposerHeightMax;
        }

        // Keep a small breathing space so the editor never exceeds its conversation region.
        return Math.Min(ComposerHeightMax, Math.Max(72, availableHeight - 32));
    }

    private void SaveLayout() => _layoutStateService.Save(_layout);

    private void ShellGrid_SizeChanged(object sender, SizeChangedEventArgs e) => ApplyLayout();

    private void MainRegion_SizeChanged(object sender, SizeChangedEventArgs e) => ApplyLayout();

    private void PrimaryContentSlot_SizeChanged(object sender, SizeChangedEventArgs e) =>
        UpdateAdaptiveContentLayout();

    private void SidebarToggle_Click(object sender, RoutedEventArgs e)
    {
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
        _dragStartValue = ComposerHost.ActualHeight;
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
        _isWorkDetailMode = false;
        ViewModel.IsChatMode = chat;
        _layout.LastPrimaryContent = chat ? "chat" : "work";
        double switcherWidth = ChatWorkSwitcher.ActualWidth > 0 ? ChatWorkSwitcher.ActualWidth : 220;
        double pillTravel = Math.Max(0, switcherWidth - PrimaryModeSelectionPill.Width);
        PrimaryModeSelectionPill.Translation = new Vector3(chat ? 0 : (float)pillTravel, 0, 8);
        ChatAmbientLayer.Visibility = chat ? Visibility.Visible : Visibility.Collapsed;
        MainContentHost.Visibility = chat ? Visibility.Visible : Visibility.Collapsed;
        WorkHistoryHost.Visibility = chat ? Visibility.Collapsed : Visibility.Visible;
        WorkDetailHost.Visibility = Visibility.Collapsed;
        WorkDetailSidebarHost.Visibility = Visibility.Collapsed;
        GlobalSidebarHeader.Visibility = Visibility.Visible;
        GlobalNavigationArea.Visibility = Visibility.Visible;
        GlobalNavigationDivider.Visibility = Visibility.Visible;
        ChatWorkSwitcher.Visibility = Visibility.Visible;
        TopWorkspaceRow.Height = new GridLength(Math.Clamp(_layout.TopWorkspaceHeight, TopMin, TopMax));
        TopWorkspaceGrip.Visibility = Visibility.Visible;
        NewPrimaryActionLabel.Text = chat ? "新对话" : "新工作";
        RecentArea.Visibility = chat && !_layout.SidebarCollapsed
            ? Visibility.Visible
            : Visibility.Collapsed;
        PromptTextBox.PlaceholderText = chat ? "向 KYNXA 提问任何问题..." : "描述你想完成的工作...";
        SaveLayout();
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

    private void WorkRow_PointerEntered(object sender, PointerRoutedEventArgs e)
    {
        WorkNavigationButton.Background =
            (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["KynxaControlHoverBrush"];
        if (_layout.SidebarCollapsed)
        {
            return;
        }

        WorkAddButton.IsHitTestVisible = true;
        WorkAddButton.Opacity = 1;
    }

    private void WorkRow_PointerExited(object sender, PointerRoutedEventArgs e)
    {
        WorkNavigationButton.Background =
            new Microsoft.UI.Xaml.Media.SolidColorBrush(Microsoft.UI.Colors.Transparent);
        WorkAddButton.Opacity = 0;
        WorkAddButton.IsHitTestVisible = false;
    }

    private void AllWorksFilterButton_Click(object sender, RoutedEventArgs e) => SetWorkFilter(null);

    private void PersonalWorksFilterButton_Click(object sender, RoutedEventArgs e) =>
        SetWorkFilter(WorkCategory.Personal);

    private void SharedWorksFilterButton_Click(object sender, RoutedEventArgs e) =>
        SetWorkFilter(WorkCategory.Shared);

    private void SetWorkFilter(WorkCategory? category)
    {
        _selectedWorkCategory = category;
        ViewModel.FilterWorks(category, WorkSearchBox.Text);
        ViewModel.SelectedWork = null;
        WorkList.SelectedItem = null;

        int filterIndex = category switch
        {
            WorkCategory.Personal => 1,
            WorkCategory.Shared => 2,
            _ => 0
        };
        double trackWidth = WorkFilterSelectionPill.Parent is FrameworkElement track && track.ActualWidth > 0
            ? track.ActualWidth
            : 234;
        double travelStep = (trackWidth - WorkFilterSelectionPill.Width) / 2;
        WorkFilterSelectionPill.Translation = new Vector3((float)(filterIndex * travelStep), 0, 8);
    }

    private void WorkSearchBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (sender is not TextBox searchBox)
        {
            return;
        }

        ViewModel.FilterWorks(_selectedWorkCategory, searchBox.Text);
        ViewModel.SelectedWork = null;
        WorkList.SelectedItem = null;
    }

    private void WorkList_ItemClick(object sender, ItemClickEventArgs e)
    {
        if (e.ClickedItem is WorkSummary work)
        {
            ViewModel.SelectedWork = work;
        }
    }

    private void WorkList_DoubleTapped(object sender, DoubleTappedRoutedEventArgs e)
    {
        if (WorkList.SelectedItem is WorkSummary work)
        {
            OpenWork(work);
            e.Handled = true;
        }
    }

    private void OpenWork(WorkSummary work)
    {
        ViewModel.SelectedWork = work;
        _isWorkDetailMode = true;

        WorkDetailNameText.Text = work.Name;
        WorkDetailTitleText.Text = work.Name;
        PromptTextBox.PlaceholderText = "向 KYNXA 提问任何问题...";

        ChatAmbientLayer.Visibility = Visibility.Visible;
        MainContentHost.Visibility = Visibility.Visible;
        WorkHistoryHost.Visibility = Visibility.Collapsed;
        WorkDetailHost.Visibility = Visibility.Collapsed;
        ChatWorkSwitcher.Visibility = Visibility.Collapsed;

        GlobalSidebarHeader.Visibility = Visibility.Collapsed;
        GlobalNavigationArea.Visibility = Visibility.Collapsed;
        GlobalNavigationDivider.Visibility = Visibility.Collapsed;
        RecentArea.Visibility = Visibility.Collapsed;
        WorkDetailSidebarHost.Visibility = Visibility.Visible;
        TopWorkspaceRow.Height = new GridLength(0);
        TopWorkspaceGrip.Visibility = Visibility.Collapsed;
    }

    private void OpenWorkMenuItem_Click(object sender, RoutedEventArgs e)
    {
        if (sender is MenuFlyoutItem { CommandParameter: WorkSummary work })
        {
            OpenWork(work);
        }
    }

    private void WorkDetailBackButton_Click(object sender, RoutedEventArgs e)
    {
        ViewModel.SelectedWork = null;
        WorkList.SelectedItem = null;
        SetPrimaryMode(true);
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

    private void ExecutionModeMenuItem_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not MenuFlyoutItem { Tag: string mode })
        {
            return;
        }

        ExecutionModeLabel.Text = mode switch
        {
            "quick" => "快速",
            "deep" => "深度",
            _ => "标准"
        };
        ToolTipService.SetToolTip(ExecutionModeButton, $"执行模式：{ExecutionModeLabel.Text}");
    }

    private void WebSearchToggleButton_StateChanged(object sender, RoutedEventArgs e)
    {
        if (sender is ToggleButton toggle)
        {
            ToolTipService.SetToolTip(
                toggle,
                toggle.IsChecked == true ? "联网搜索：已开启" : "联网搜索：已关闭");
        }
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

    private void SendButton_Click(object sender, RoutedEventArgs e)
    {
        if (string.IsNullOrWhiteSpace(ViewModel.Prompt))
        {
            PromptTextBox.Focus(FocusState.Programmatic);
        }
    }
}
