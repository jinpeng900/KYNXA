using KYNXA_Desktop.ViewModels;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
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
    private const double ComposerHeightMax = 260;

    private readonly LayoutStateService _layoutStateService = new();
    private LayoutState _layout = LayoutState.CreateDefault();
    private double _dragStartValue;

    public ShellViewModel ViewModel { get; } = new();

    public ShellPage()
    {
        InitializeComponent();
    }

    private void PageRoot_Loaded(object sender, RoutedEventArgs e)
    {
        _layout = _layoutStateService.Load();
        ApplyLayout();
        SetPrimaryMode(_layout.LastPrimaryContent != "work");
    }

    private void ApplyLayout()
    {
        double sidebar = _layout.SidebarCollapsed
            ? SidebarCollapsed
            : Math.Clamp(_layout.SidebarWidth, SidebarMin, SidebarMax);

        SidebarColumn.Width = new GridLength(sidebar);
        RecentArea.Visibility = _layout.SidebarCollapsed ? Visibility.Collapsed : Visibility.Visible;
        TopWorkspaceRow.Height = new GridLength(Math.Clamp(_layout.TopWorkspaceHeight, TopMin, TopMax));

        double available = PrimaryContentSlot.ActualWidth > 0 ? PrimaryContentSlot.ActualWidth : MainRegion.ActualWidth;
        double maximumComposerWidth = Math.Min(ComposerWidthMax, Math.Max(ComposerWidthMin, available - 96));
        ComposerHost.Width = Math.Clamp(_layout.ComposerWidth, ComposerWidthMin, maximumComposerWidth);
        ComposerHost.Height = Math.Clamp(_layout.ComposerHeight, ComposerHeightMin, ComposerHeightMax);
        UpdateVisualAxis();
    }

    private void UpdateVisualAxis()
    {
        // The approved composition is centered at 47.7% of the main region.
        float offset = (float)(MainRegion.ActualWidth * -0.023);
        ChatWorkSwitcher.Translation = new Vector3(offset, 0, 16);
        MainContentHost.Translation = new Vector3(offset, 0, 0);
    }

    private void SaveLayout() => _layoutStateService.Save(_layout);

    private void ShellGrid_SizeChanged(object sender, SizeChangedEventArgs e) => ApplyLayout();

    private void MainRegion_SizeChanged(object sender, SizeChangedEventArgs e) => ApplyLayout();

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
        double max = Math.Min(ComposerWidthMax, Math.Max(ComposerWidthMin, PrimaryContentSlot.ActualWidth - 96));
        _layout.ComposerWidth = Math.Clamp(requested, ComposerWidthMin, max);
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

    private void ComposerHeightGrip_DragStarted(object? sender, EventArgs e) =>
        _dragStartValue = ComposerHost.ActualHeight;

    private void ComposerTopGrip_DragDelta(object? sender, ResizeDeltaEventArgs e)
    {
        _layout.ComposerHeight = Math.Clamp(_dragStartValue - e.Delta, ComposerHeightMin, ComposerHeightMax);
        ApplyLayout();
    }

    private void ComposerHeightGrip_CancelRequested(object? sender, EventArgs e)
    {
        _layout.ComposerHeight = _dragStartValue;
        ApplyLayout();
    }

    private void ComposerHeightGrip_ResetRequested(object? sender, EventArgs e)
    {
        _layout.ComposerHeight = ComposerHeightDefault;
        ApplyLayout();
        SaveLayout();
    }

    private void LayoutGrip_DragCompleted(object? sender, EventArgs e) => SaveLayout();

    private void ChatModeButton_Click(object sender, RoutedEventArgs e) => SetPrimaryMode(true);

    private void WorkModeButton_Click(object sender, RoutedEventArgs e) => SetPrimaryMode(false);

    private void SetPrimaryMode(bool chat)
    {
        ViewModel.IsChatMode = chat;
        _layout.LastPrimaryContent = chat ? "chat" : "work";
        ChatModeButton.Background = chat ? new SolidColorBrush(Microsoft.UI.Colors.White) : new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        WorkModeButton.Background = chat ? new SolidColorBrush(Microsoft.UI.Colors.Transparent) : new SolidColorBrush(Microsoft.UI.Colors.White);
        PromptTextBox.PlaceholderText = chat ? "向 KYNXA 提问任何问题..." : "描述你想完成的工作...";
        SaveLayout();
    }

    private void NewConversation_Click(object sender, RoutedEventArgs e)
    {
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
