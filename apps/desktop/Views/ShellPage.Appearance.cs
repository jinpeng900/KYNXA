using KYNXA_Desktop.Controls;
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
        => TrySetAppearancePreferences(new(paletteId, AppearanceService.Mode,
            AppearanceService.ChatFontSize, AppearanceService.CustomAccent), out errorKey);

    private sealed record AppearancePreferences(string? PaletteId, string? Mode, double FontSize, string? Accent);

    private bool TrySetAppearancePreferences(AppearancePreferences preferences, out string? errorKey)
    {
        errorKey = null;
        if (StoragePaths.IsMigrating)
        {
            errorKey = "正在迁移数据，请稍后更改外观。";
            return false;
        }

        var selected = new AppearancePreferences(AppearanceService.NormalizePaletteId(preferences.PaletteId),
            AppearanceService.NormalizeMode(preferences.Mode), AppearanceService.NormalizeChatFontSize(preferences.FontSize),
            AppearanceService.NormalizeAccent(preferences.Accent));
        var previous = new AppearancePreferences(_layout.AppearancePaletteId, _layout.AppearanceMode,
            _layout.ChatFontSize, _layout.AppearanceAccent);
        _layout.AppearancePaletteId = selected.PaletteId!;
        _layout.AppearanceMode = selected.Mode!;
        _layout.ChatFontSize = selected.FontSize;
        _layout.AppearanceAccent = selected.Accent;
        if (!_layoutStateService.Save(_layout))
        {
            _layout.AppearancePaletteId = previous.PaletteId!;
            _layout.AppearanceMode = previous.Mode!;
            _layout.ChatFontSize = previous.FontSize;
            _layout.AppearanceAccent = previous.Accent;
            errorKey = "配色未能保存，已保留原来的外观，请稍后重试。";
            return false;
        }

        // Apply only after the atomic preference save succeeds; a failed save keeps the visible palette intact.
        // 偏好原子保存成功后再切换画刷；保存失败时保留当前可见配色。
        AppearanceService.ApplyPreferences(selected.PaletteId, selected.Mode, selected.FontSize, selected.Accent);
        return true;
    }

    private sealed partial class AppearanceSettingsSection : IDisposable
    {
        private sealed record PaletteCard(AppearancePalette Palette, Button Button, Border Frame, TextBlock Check);

        private readonly Func<AppearancePreferences, string?> _savePreferences;
        private readonly List<PaletteCard> _cards = [];
        private readonly Grid _paletteGrid = new() { ColumnSpacing = 10, RowSpacing = 10 };
        private readonly InfoBar _status = new() { IsOpen = false, IsClosable = false };
        private readonly Button _resetButton;
        private readonly ComboBox _modePicker;
        private readonly ComboBox _fontPicker;
        private readonly TextBox _accentInput;
        private readonly Button _accentReset;
        private bool _refreshing;
        private bool _accentDirty;
        private int _columnCount;
        private bool _isDisposed;

        public StackPanel Root { get; } = new() { Spacing = 10, Margin = new Thickness(0, 8, 0, 8) };

        public AppearanceSettingsSection(FontFamily font, Func<AppearancePreferences, string?> savePreferences,
            Func<AppearanceTransferPreferences> readSavedPreferences, Func<bool, Task<Windows.Storage.StorageFile?>> pickFile)
        {
            _savePreferences = savePreferences;
            AutomationProperties.SetAutomationId(Root, "AppearancePaletteSection");
            var heading = new Grid { ColumnSpacing = 12 };
            heading.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            heading.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            var title = new TextBlock { FontFamily = font, FontSize = 13, VerticalAlignment = VerticalAlignment.Center,
                Foreground = AppearanceService.GetBrush("KynxaSecondaryTextBrush") };
            UiLocalization.Bind(title, TextBlock.TextProperty, "外观");
            _resetButton = new Button { FontFamily = font, FontSize = 12, Padding = new Thickness(10, 5, 10, 5),
                CornerRadius = new CornerRadius(8), Style = (Style)Application.Current.Resources["KynxaQuietButtonStyle"] };
            UiLocalization.Bind(_resetButton, Button.ContentProperty, "恢复默认外观");
            UiLocalization.Bind(_resetButton, AutomationProperties.NameProperty, "恢复默认外观");
            AutomationProperties.SetAutomationId(_resetButton, "AppearanceResetButton");
            _resetButton.Click += (_, _) => Save(new(AppearanceService.DefaultPaletteId, "light", 15, null), resetAccentDraft: true);
            Grid.SetColumn(_resetButton, 1);
            heading.Children.Add(title);
            heading.Children.Add(_resetButton);
            Root.Children.Add(heading);
            _modePicker = new ScrollSafeComboBox { FontFamily = font, HorizontalAlignment = HorizontalAlignment.Stretch };
            AutomationProperties.SetAutomationId(_modePicker, "AppearanceModePicker");
            UiLocalization.Bind(_modePicker, AutomationProperties.NameProperty, "主题模式");
            foreach (var (id, key) in new[] { ("light", "浅色"), ("dark", "深色"), ("system", "跟随系统") })
            {
                var item = new ComboBoxItem { Tag = id };
                UiLocalization.Bind(item, ComboBoxItem.ContentProperty, key);
                _modePicker.Items.Add(item);
            }
            Root.Children.Add(BuildSettingRow(font, "主题模式", _modePicker));
            _modePicker.SelectionChanged += (_, _) =>
            {
                if (!_refreshing && _modePicker.SelectedItem is ComboBoxItem { Tag: string mode })
                    Save(CurrentPreferences() with { Mode = mode });
            };
            _fontPicker = new ScrollSafeComboBox { FontFamily = font, HorizontalAlignment = HorizontalAlignment.Stretch };
            AutomationProperties.SetAutomationId(_fontPicker, "ChatFontSizePicker");
            UiLocalization.Bind(_fontPicker, AutomationProperties.NameProperty, "聊天字号");
            foreach (double size in Enumerable.Range(26, 19).Select(value => value / 2d))
                _fontPicker.Items.Add(new ComboBoxItem { Content = size.ToString("0.#", System.Globalization.CultureInfo.InvariantCulture) + " px", Tag = size });
            Root.Children.Add(BuildSettingRow(font, "聊天字号", _fontPicker));
            _fontPicker.SelectionChanged += (_, _) =>
            {
                if (!_refreshing && _fontPicker.SelectedItem is ComboBoxItem { Tag: double size })
                    Save(CurrentPreferences() with { FontSize = size });
            };
            var readingHint = new TextBlock { FontFamily = font, FontSize = 12, TextWrapping = TextWrapping.Wrap,
                Foreground = AppearanceService.GetBrush("KynxaSecondaryTextBrush") };
            UiLocalization.Bind(readingHint, TextBlock.TextProperty, "调整聊天正文、代码与表格字号，界面按钮大小保持不变。");
            Root.Children.Add(readingHint);
            var description = new TextBlock { FontFamily = font, FontSize = 12, TextWrapping = TextWrapping.Wrap,
                Foreground = AppearanceService.GetBrush("KynxaSecondaryTextBrush") };
            UiLocalization.Bind(description, TextBlock.TextProperty, "选择配色，适配当前主题并自动记住。");
            Root.Children.Add(description);

            foreach (var palette in AppearanceService.Palettes)
            {
                var card = BuildCard(palette, font);
                card.Button.Click += (_, _) => SelectPalette(palette.Id);
                _cards.Add(card);
                _paletteGrid.Children.Add(card.Button);
            }
            Root.Children.Add(_paletteGrid);
            _accentInput = new TextBox { FontFamily = font, PlaceholderText = "#4568B2", MaxLength = 7,
                HorizontalAlignment = HorizontalAlignment.Stretch };
            AutomationProperties.SetAutomationId(_accentInput, "AppearanceAccentInput");
            UiLocalization.Bind(_accentInput, AutomationProperties.NameProperty, "自定义强调色");
            var accentRow = new Grid { ColumnSpacing = 8 };
            accentRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            accentRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            accentRow.Children.Add(_accentInput);
            var chooseColor = new Button { FontFamily = font, CornerRadius = new CornerRadius(8) };
            UiLocalization.Bind(chooseColor, Button.ContentProperty, "选择颜色");
            UiLocalization.Bind(chooseColor, AutomationProperties.NameProperty, "选择颜色");
            AutomationProperties.SetAutomationId(chooseColor, "AppearanceColorPickerButton");
            var colorPicker = new ColorPicker { IsAlphaEnabled = false, Width = 280 };
            AutomationProperties.SetAutomationId(colorPicker, "AppearanceColorPicker");
            UiLocalization.Bind(colorPicker, AutomationProperties.NameProperty, "自定义强调色");
            var colorFlyout = new Flyout { Content = colorPicker };
            colorFlyout.Opening += (_, _) =>
            {
                bool wasRefreshing = _refreshing;
                _refreshing = true;
                try
                {
                    colorPicker.Color = AppearanceService.ParseColor(
                        AppearanceService.NormalizeAccent(_accentInput.Text) ?? AppearanceService.Current.Accent);
                }
                finally { _refreshing = wasRefreshing; }
            };
            colorPicker.ColorChanged += (_, args) =>
            {
                if (!_refreshing) _accentInput.Text = $"#{args.NewColor.R:X2}{args.NewColor.G:X2}{args.NewColor.B:X2}";
            };
            chooseColor.Flyout = colorFlyout;
            Grid.SetColumn(chooseColor, 1);
            accentRow.Children.Add(chooseColor);
            Root.Children.Add(BuildSettingRow(font, "自定义强调色", accentRow));
            var accentActions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
            var applyAccent = new Button { FontFamily = font, CornerRadius = new CornerRadius(8) };
            UiLocalization.Bind(applyAccent, Button.ContentProperty, "应用颜色");
            AutomationProperties.SetAutomationId(applyAccent, "AppearanceAccentApplyButton");
            applyAccent.Click += (_, _) => ApplyAccent();
            _accentReset = new Button { FontFamily = font, CornerRadius = new CornerRadius(8) };
            UiLocalization.Bind(_accentReset, Button.ContentProperty, "使用预设颜色");
            AutomationProperties.SetAutomationId(_accentReset, "AppearanceAccentResetButton");
            _accentReset.Click += (_, _) => Save(CurrentPreferences() with { Accent = null }, resetAccentDraft: true);
            // TextChanging is synchronous; TextChanged can arrive after a programmatic refresh has ended.
            // TextChanging 同步执行，避免程序刷新结束后异步 TextChanged 重新标记草稿。
            _accentInput.TextChanging += (_, _) =>
            {
                if (!_refreshing) _accentDirty = true;
            };
            _accentInput.TextChanged += (_, _) => UpdateResetAvailability();
            accentActions.Children.Add(applyAccent);
            accentActions.Children.Add(_accentReset);
            Root.Children.Add(accentActions);
            var accentHint = new TextBlock { FontFamily = font, FontSize = 12, TextWrapping = TextWrapping.Wrap,
                Foreground = AppearanceService.GetBrush("KynxaSecondaryTextBrush") };
            UiLocalization.Bind(accentHint, TextBlock.TextProperty, "输入 #RRGGBB；显示时会自动调整对比度。系统高对比度优先。");
            Root.Children.Add(accentHint);
            AddTransferControls(font, readSavedPreferences, pickFile);
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

        private static StackPanel BuildSettingRow(FontFamily font, string key, FrameworkElement control)
        {
            var label = new TextBlock { FontFamily = font, FontSize = 13,
                Foreground = AppearanceService.GetBrush("KynxaTextBrush") };
            UiLocalization.Bind(label, TextBlock.TextProperty, key);
            var row = new StackPanel { Spacing = 6, Padding = new Thickness(12), CornerRadius = new CornerRadius(10),
                Background = AppearanceService.GetBrush("KynxaSettingsCardBrush") };
            row.Children.Add(label);
            row.Children.Add(control);
            return row;
        }

        private static AppearancePreferences CurrentPreferences() => new(AppearanceService.Current.Id,
            AppearanceService.Mode, AppearanceService.ChatFontSize, AppearanceService.CustomAccent);

        private void ApplyAccent()
        {
            string value = _accentInput.Text.Trim();
            string? accent = AppearanceService.NormalizeAccent(value);
            if (value.Length > 0 && accent is null)
            {
                UiLocalization.Bind(_status, InfoBar.MessageProperty, "请输入有效颜色，例如 #4568B2。");
                _status.Severity = InfoBarSeverity.Error;
                _status.IsOpen = true;
                return;
            }
            Save(CurrentPreferences() with { Accent = accent }, resetAccentDraft: true);
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
                Foreground = AppearanceService.GetBrush("KynxaAccentTextBrush") };
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
            => Save(CurrentPreferences() with { PaletteId = paletteId });

        private void Save(AppearancePreferences preferences, bool resetAccentDraft = false)
        {
            if (_isDisposed) return;
            string? errorKey = _savePreferences(preferences);
            _status.IsOpen = errorKey is not null;
            if (errorKey is not null)
            {
                UiLocalization.Bind(_status, InfoBar.MessageProperty, errorKey);
                _status.Severity = InfoBarSeverity.Error;
            }
            // Keep an unapplied HEX draft through theme/font/language refreshes, including failed saves.
            // 主题、字号和语言刷新以及保存失败时，保留尚未应用的 HEX 草稿。
            if (errorKey is null && resetAccentDraft) _accentDirty = false;
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
                card.Frame.BorderBrush = AppearanceService.GetBrush(selected ? "KynxaFocusBrush" : "KynxaDividerBrush");
                card.Check.Opacity = selected ? 1 : 0;
                AutomationProperties.SetName(card.Button, UiText.Get(card.Palette.NameKey));
                AutomationProperties.SetHelpText(card.Button, UiText.Get(selected ? "已选中" : "未选中"));
                AutomationProperties.SetItemStatus(card.Button, UiText.Get(selected ? "已选中" : "未选中"));
            }
            _refreshing = true;
            try
            {
                var selectedMode = _modePicker.Items.Cast<ComboBoxItem>().First(item => (string)item.Tag == AppearanceService.Mode);
                // Refresh WinUI's cached selection text after localization without treating it as a user preference change.
                // 本地化后刷新 WinUI 缓存的选中显示；刷新守卫阻止将同项重选当成用户偏好变更。
                _modePicker.SelectedItem = null;
                _modePicker.SelectedItem = selectedMode;
                _fontPicker.SelectedItem = _fontPicker.Items.Cast<ComboBoxItem>().First(item => (double)item.Tag == AppearanceService.ChatFontSize);
                if (!_accentDirty) _accentInput.Text = AppearanceService.CustomAccent ?? string.Empty;
            }
            finally { _refreshing = false; }
            UpdateResetAvailability();
        }

        private void UpdateResetAvailability()
        {
            _accentReset.IsEnabled = _accentDirty || AppearanceService.CustomAccent is not null;
            _resetButton.IsEnabled = _accentDirty || CurrentPreferences() != new AppearancePreferences(AppearanceService.DefaultPaletteId, "light", 15, null);
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
