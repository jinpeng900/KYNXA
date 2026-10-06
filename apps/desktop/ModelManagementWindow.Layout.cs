using KYNXA_Desktop.Services;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop;

public sealed partial class ModelManagementWindow
{
    private bool _updatingConnectionPicker;
    private bool? _compactLayout;
    private bool? _stackTokenValues;
    private XamlRoot? _windowXamlRoot;

    private void WindowRoot_Loaded(object sender, RoutedEventArgs e)
    {
        if (_windowXamlRoot is not null) return;
        _windowXamlRoot = WindowRoot.XamlRoot;
        if (_windowXamlRoot is null) return;
        _windowXamlRoot.Changed += WindowXamlRoot_Changed;
        UpdateWindowMinimumSize();
        UpdateResponsiveLayout();
    }

    private void WindowXamlRoot_Changed(XamlRoot sender, XamlRootChangedEventArgs args) => UpdateWindowMinimumSize();

    private void UpdateWindowMinimumSize()
    {
        if (AppWindow.Presenter is not OverlappedPresenter presenter) return;
        double scale = _windowXamlRoot?.RasterizationScale ?? 1;
        presenter.PreferredMinimumWidth = (int)Math.Ceiling(480 * scale);
        presenter.PreferredMinimumHeight = (int)Math.Ceiling(440 * scale);
    }

    private void ManagementLayout_SizeChanged(object sender, SizeChangedEventArgs e) => UpdateResponsiveLayout();

    private void UpdateResponsiveLayout()
    {
        if (EditorPanel is null || ManagementLayout.ActualWidth <= 0) return;
        bool compact = ManagementLayout.ActualWidth < 760;
        if (_compactLayout != compact)
        {
            _compactLayout = compact;
            ConnectionsColumn.Width = new GridLength(compact ? 0 : 212);
            CompactConnectionsRow.Height = compact ? GridLength.Auto : new GridLength(0);
            Grid.SetRow(ProviderPanel, compact ? 0 : 1);
            Grid.SetColumn(ProviderPanel, compact ? 1 : 0);
            ConnectionsHeading.Visibility = SavedConnectionsList.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
            CompactConnectionPicker.Visibility = compact ? Visibility.Visible : Visibility.Collapsed;
            Grid.SetRow(NewConnectionButton, compact ? 1 : 2);
            Grid.SetColumn(NewConnectionButton, compact ? 1 : 0);
            Grid.SetColumnSpan(NewConnectionButton, compact ? 1 : 2);
            ProviderPanel.Padding = compact ? new Thickness(16, 10, 16, 10) : new Thickness(16, 20, 12, 18);
            EditorPanel.Padding = compact ? new Thickness(16, 14, 16, 14) : new Thickness(28, 22, 28, 18);
            EditorPanel.RowSpacing = compact ? 12 : 16;
            EditorForm.Margin = compact ? new Thickness(0, 0, 0, 8) : new Thickness(0, 0, 12, 8);
            EditorTitle.FontSize = compact ? 20 : 22;
        }
        bool stackValues = ManagementLayout.ActualWidth < 620;
        if (_stackTokenValues == stackValues) return;
        _stackTokenValues = stackValues;
        ArrangeTokenRow(ContextWindowBox, CustomContextWindowBox, ContextWindowRow, stackValues);
        ArrangeTokenRow(MaxOutputTokensBox, CustomMaxOutputTokensBox, MaxOutputTokensRow, stackValues);
        // Move existing controls instead of rebuilding the form, so drafts, focus and scroll survive resize.
        // 移动已有控件而不重建表单，使草稿、焦点和滚动位置在缩放时保留。
    }

    private static void ArrangeTokenRow(ComboBox choice, TextBox customValue, Grid row, bool stackValues)
    {
        Grid.SetColumnSpan(choice, stackValues ? 2 : 1);
        Grid.SetRow(customValue, stackValues ? 1 : 0);
        Grid.SetColumn(customValue, stackValues ? 0 : 1);
        Grid.SetColumnSpan(customValue, stackValues ? 2 : 1);
        customValue.Width = stackValues ? double.NaN : 154;
        customValue.HorizontalAlignment = HorizontalAlignment.Stretch;
        row.RowSpacing = stackValues ? 8 : 0;
    }

    private void SyncConnectionPicker()
    {
        _updatingConnectionPicker = true;
        try
        {
            CompactConnectionPicker.ItemsSource = _providers;
            CompactConnectionPicker.SelectedItem = _providers.FirstOrDefault(provider => provider.ProviderId == _editing?.ProviderId);
            CompactConnectionPicker.PlaceholderText = UiText.Get(_editing is null ? "添加新的连接" : "选择已保存连接");
            ToolTipService.SetToolTip(CompactConnectionPicker, _editing?.DisplayName);
        }
        finally { _updatingConnectionPicker = false; }
    }

    private async void CompactConnectionPicker_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_updatingConnectionPicker || CompactConnectionPicker.SelectedItem is not ModelProvider provider ||
            _editing?.ProviderId == provider.ProviderId) return;
        SyncConnectionPicker();
        if (!_busy && await ConfirmDiscardAsync("切换连接")) SelectProvider(provider);
    }
}
