using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private bool TrySetAppearancePalette(string? paletteId, out string? errorKey)
    {
        errorKey = null;
        if (StoragePaths.IsMigrating)
        {
            errorKey = "正在迁移数据，请稍后更改配色。";
            return false;
        }

        string selectedId = AppearanceService.NormalizePaletteId(paletteId);
        if (_layout.AppearancePaletteId == selectedId && AppearanceService.Current.Id == selectedId) return true;
        string previousId = _layout.AppearancePaletteId;
        _layout.AppearancePaletteId = selectedId;
        if (!_layoutStateService.Save(_layout))
        {
            _layout.AppearancePaletteId = previousId;
            errorKey = "配色未能保存，已保留原来的外观，请稍后重试。";
            return false;
        }

        // Apply only after the atomic preference save succeeds; a failed save keeps the visible palette intact.
        // 偏好原子保存成功后再切换画刷；保存失败时保留当前可见配色。
        AppearanceService.Apply(selectedId);
        return true;
    }

    private sealed class AppearanceSettingsSection : IDisposable
    {
        private sealed record PaletteCard(AppearancePalette Palette, Button Button, Border Frame, TextBlock Check);

        private readonly Func<string, string?> _savePalette;
        private readonly List<PaletteCard> _cards = [];
        private readonly Grid _paletteGrid = new() { ColumnSpacing = 10, RowSpacing = 10 };
        private readonly InfoBar _status = new() { IsOpen = false, IsClosable = false };
        private readonly Button _resetButton;
        private int _columnCount;
        private bool _isDisposed;

        public StackPanel Root { get; } = new() { Spacing = 10, Margin = new Thickness(0, 8, 0, 8) };

        public AppearanceSettingsSection(FontFamily font, Func<string, string?> savePalette)
        {
            _savePalette = savePalette;
            AutomationProperties.SetAutomationId(Root, "AppearancePaletteSection");
            var heading = new Grid { ColumnSpacing = 12 };
            heading.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            heading.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            var title = new TextBlock { FontFamily = font, FontSize = 13, VerticalAlignment = VerticalAlignment.Center,
                Foreground = AppearanceService.GetBrush("KynxaSecondaryTextBrush") };
            UiLocalization.Bind(title, TextBlock.TextProperty, "外观");
            _resetButton = new Button { FontFamily = font, FontSize = 12, Padding = new Thickness(10, 5, 10, 5),
                CornerRadius = new CornerRadius(8), Style = (Style)Application.Current.Resources["KynxaQuietButtonStyle"] };
            UiLocalization.Bind(_resetButton, Button.ContentProperty, "恢复默认配色");
            UiLocalization.Bind(_resetButton, AutomationProperties.NameProperty, "恢复默认配色");
            AutomationProperties.SetAutomationId(_resetButton, "AppearanceResetButton");
            _resetButton.Click += (_, _) => SelectPalette(AppearanceService.DefaultPaletteId);
            Grid.SetColumn(_resetButton, 1);
            heading.Children.Add(title);
            heading.Children.Add(_resetButton);
            Root.Children.Add(heading);
            var description = new TextBlock { FontFamily = font, FontSize = 12, TextWrapping = TextWrapping.Wrap,
                Foreground = AppearanceService.GetBrush("KynxaSecondaryTextBrush") };
            UiLocalization.Bind(description, TextBlock.TextProperty, "选择一套浅色配色，立即应用并自动记住。");
            Root.Children.Add(description);

            foreach (var palette in AppearanceService.Palettes)
            {
                var card = BuildCard(palette, font);
                card.Button.Click += (_, _) => SelectPalette(palette.Id);
                _cards.Add(card);
                _paletteGrid.Children.Add(card.Button);
            }
            Root.Children.Add(_paletteGrid);
            AutomationProperties.SetAutomationId(_paletteGrid, "AppearancePaletteGrid");
            AutomationProperties.SetAutomationId(_status, "AppearanceSettingsStatus");
            AutomationProperties.SetLiveSetting(_status, Microsoft.UI.Xaml.Automation.Peers.AutomationLiveSetting.Polite);
            Root.Children.Add(_status);
            _paletteGrid.SizeChanged += PaletteGridSizeChanged;
            AppearanceService.Changed += RefreshSelection;
            UiText.LanguageChanged += RefreshSelection;
            UpdateColumns(2);
            RefreshSelection(null, EventArgs.Empty);
        }

        private static PaletteCard BuildCard(AppearancePalette palette, FontFamily font)
        {
            SolidColorBrush Brush(string color) => new(AppearanceService.ParseColor(color));
            var preview = new Grid { Height = 62, Background = Brush(palette.Main), CornerRadius = new CornerRadius(7),
                BorderBrush = Brush(palette.Border), BorderThickness = new Thickness(1), ColumnSpacing = 8 };
            preview.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(28) });
            preview.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            var sidebar = new StackPanel { Spacing = 5, Padding = new Thickness(5, 9, 5, 0), Background = Brush(palette.Sidebar),
                CornerRadius = new CornerRadius(6, 0, 0, 6) };
            sidebar.Children.Add(new Border { Height = 5, Background = Brush(palette.Accent), CornerRadius = new CornerRadius(3) });
            sidebar.Children.Add(new Border { Height = 4, Background = Brush(palette.Border), CornerRadius = new CornerRadius(2) });
            sidebar.Children.Add(new Border { Height = 4, Background = Brush(palette.Border), CornerRadius = new CornerRadius(2) });
            preview.Children.Add(sidebar);
            var messages = new StackPanel { Spacing = 6, Margin = new Thickness(0, 9, 8, 0) };
            messages.Children.Add(new Border { Height = 12, Width = 40, HorizontalAlignment = HorizontalAlignment.Right,
                Background = Brush(palette.Soft), CornerRadius = new CornerRadius(4) });
            messages.Children.Add(new Border { Height = 4, Width = 44, HorizontalAlignment = HorizontalAlignment.Left,
                Background = Brush(palette.Border), CornerRadius = new CornerRadius(2) });
            messages.Children.Add(new Border { Height = 12, Width = 12, HorizontalAlignment = HorizontalAlignment.Right,
                Background = Brush(palette.Accent), CornerRadius = new CornerRadius(6) });
            Grid.SetColumn(messages, 1);
            preview.Children.Add(messages);
            AutomationProperties.SetAccessibilityView(preview, Microsoft.UI.Xaml.Automation.Peers.AccessibilityView.Raw);

            var nameRow = new Grid { ColumnSpacing = 6 };
            nameRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            nameRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            var name = new TextBlock { FontFamily = font, FontSize = 13,
                Foreground = AppearanceService.GetBrush("KynxaTextBrush") };
            UiLocalization.Bind(name, TextBlock.TextProperty, palette.NameKey);
            var check = new TextBlock { Text = "\uE73E", FontFamily = new FontFamily("Segoe Fluent Icons"), FontSize = 13,
                Width = 16, VerticalAlignment = VerticalAlignment.Center,
                Foreground = AppearanceService.GetBrush("KynxaAccentBrush") };
            Grid.SetColumn(check, 1);
            nameRow.Children.Add(name);
            nameRow.Children.Add(check);
            var body = new StackPanel { Spacing = 8 };
            body.Children.Add(preview);
            body.Children.Add(nameRow);
            var frame = new Border { Padding = new Thickness(9), CornerRadius = new CornerRadius(11),
                BorderThickness = new Thickness(2), Child = body };
            var button = new Button { Content = frame, Padding = new Thickness(0), Margin = new Thickness(0), MinWidth = 0,
                HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Stretch,
                Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent), BorderThickness = new Thickness(0),
                CornerRadius = new CornerRadius(11), UseSystemFocusVisuals = true };
            button.Resources["SystemControlFocusVisualPrimaryBrush"] = AppearanceService.GetBrush("KynxaFocusBrush");
            AutomationProperties.SetAutomationId(button, "AppearancePalette_" + palette.Id);
            return new PaletteCard(palette, button, frame, check);
        }

        private void SelectPalette(string paletteId)
        {
            if (_isDisposed) return;
            string? errorKey = _savePalette(paletteId);
            _status.IsOpen = errorKey is not null;
            if (errorKey is not null)
            {
                UiLocalization.Bind(_status, InfoBar.MessageProperty, errorKey);
                _status.Severity = InfoBarSeverity.Error;
            }
            RefreshSelection(null, EventArgs.Empty);
        }

        private void RefreshSelection(object? sender, EventArgs args)
        {
            if (_isDisposed) return;
            if (!Root.DispatcherQueue.HasThreadAccess)
            {
                Root.DispatcherQueue.TryEnqueue(() => RefreshSelection(sender, args));
                return;
            }
            foreach (var card in _cards)
            {
                bool selected = card.Palette.Id == AppearanceService.Current.Id;
                card.Frame.Background = AppearanceService.GetBrush(selected ? "KynxaSelectionBrush" : "KynxaMainBrush");
                card.Frame.BorderBrush = AppearanceService.GetBrush(selected ? "KynxaAccentBrush" : "KynxaDividerBrush");
                card.Check.Opacity = selected ? 1 : 0;
                AutomationProperties.SetName(card.Button, UiText.Get(card.Palette.NameKey));
                AutomationProperties.SetHelpText(card.Button, UiText.Get(selected ? "已选中" : "未选中"));
                AutomationProperties.SetItemStatus(card.Button, UiText.Get(selected ? "已选中" : "未选中"));
            }
            _resetButton.IsEnabled = AppearanceService.Current.Id != AppearanceService.DefaultPaletteId;
        }

        private void PaletteGridSizeChanged(object sender, SizeChangedEventArgs args)
        {
            int columns = args.NewSize.Width >= 450 ? 3 : args.NewSize.Width >= 260 ? 2 : 1;
            if (columns != _columnCount) UpdateColumns(columns);
        }

        private void UpdateColumns(int columns)
        {
            _columnCount = columns;
            _paletteGrid.ColumnDefinitions.Clear();
            _paletteGrid.RowDefinitions.Clear();
            for (int column = 0; column < columns; column++)
                _paletteGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            for (int row = 0; row < (int)Math.Ceiling((double)_cards.Count / columns); row++)
                _paletteGrid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            for (int index = 0; index < _cards.Count; index++)
            {
                Grid.SetColumn(_cards[index].Button, index % columns);
                Grid.SetRow(_cards[index].Button, index / columns);
            }
        }

        public void Dispose()
        {
            if (_isDisposed) return;
            _isDisposed = true;
            AppearanceService.Changed -= RefreshSelection;
            UiText.LanguageChanged -= RefreshSelection;
            _paletteGrid.SizeChanged -= PaletteGridSizeChanged;
        }
    }
}
