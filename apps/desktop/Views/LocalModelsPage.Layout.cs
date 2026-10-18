using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Markup;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Views;

public sealed partial class LocalModelsPage
{
    private readonly Grid _root = new();
    private readonly ToggleSwitch _enabled = new();
    private readonly ScrollSafeComboBox _policy = new();
    private readonly ScrollSafeComboBox _filter = new();
    private readonly TextBox _search = new();
    private readonly TextBlock _directory = new() { IsTextSelectionEnabled = true, TextTrimming = TextTrimming.CharacterEllipsis, FontSize = 12 };
    private readonly TextBlock _storageSpace = new() { TextWrapping = TextWrapping.Wrap, FontSize = 12, Opacity = 0.7 };
    private readonly TextBlock _hardware = new() { TextWrapping = TextWrapping.Wrap, FontSize = 12, Opacity = 0.7 };
    private readonly TextBlock _count = new() { FontSize = 12, Opacity = 0.65 };
    private readonly TextBlock _empty = new() { TextWrapping = TextWrapping.Wrap, HorizontalAlignment = HorizontalAlignment.Center,
        VerticalAlignment = VerticalAlignment.Center, Opacity = 0.65 };
    private readonly InfoBar _notice = new() { IsOpen = false, IsClosable = true };
    private readonly Button _changeDirectory = new();
    private readonly Button _import = new();
    private readonly ListView _models = new() { SelectionMode = ListViewSelectionMode.None, IsItemClickEnabled = false,
        HorizontalContentAlignment = HorizontalAlignment.Stretch, Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent) };

    private void BuildLayout()
    {
        _root.Background = AppearanceService.GetBrush("KynxaMainBrush");
        ApplyNeutralResources(_root.Resources);
        _root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        _root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        _root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        var navigation = new Grid { ColumnSpacing = 8, Margin = new Thickness(20, 8, 20, 0) };
        navigation.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        navigation.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        navigation.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var back = new Button
        {
            Content = new SymbolIcon(Symbol.Back),
            Width = 32, Height = 32, Padding = new Thickness(8), CornerRadius = new CornerRadius(8),
            Style = (Style)Application.Current.Resources["KynxaQuietButtonStyle"],
            FocusVisualPrimaryBrush = AppearanceService.GetBrush("KynxaSecondaryTextBrush")
        };
        UiLocalization.Bind(back, AutomationProperties.NameProperty, "返回设置");
        UiLocalization.Bind(back, ToolTipService.ToolTipProperty, "返回设置");
        AutomationProperties.SetAutomationId(back, "LocalModelsBack");
        back.Click += (_, _) => BackRequested?.Invoke(this, EventArgs.Empty);
        navigation.Children.Add(back);
        _root.Children.Add(navigation);
        var settings = new StackPanel { Spacing = 8, Margin = new Thickness(20, 8, 20, 8) };
        settings.Children.Add(_notice);
        AutomationProperties.SetAutomationId(_notice, "LocalModelsNotice");
        var card = new StackPanel { Spacing = 6 };
        _enabled.IsEnabled = false;
        _enabled.FontFamily = UIFont;
        UiLocalization.Bind(_enabled, ToggleSwitch.OnContentProperty, "启用");
        UiLocalization.Bind(_enabled, ToggleSwitch.OffContentProperty, "停用");
        UiLocalization.Bind(_enabled, AutomationProperties.NameProperty, "本地模型总开关");
        AutomationProperties.SetAutomationId(_enabled, "LocalModelsEnabled");
        _enabled.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(_enabled, 2);
        navigation.Children.Add(_enabled);
        UiLocalization.Bind(_enabled, ToolTipService.ToolTipProperty, "无需 API 密钥；启用后可在聊天模型列表选择。");
        var storage = new Grid { ColumnSpacing = 8 };
        storage.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        storage.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var location = new StackPanel { Spacing = 3 };
        location.Children.Add(Label("模型安装目录", 12, secondary: true));
        _directory.FontFamily = UIFont;
        AutomationProperties.SetAutomationId(_directory, "LocalModelsInstallDirectory");
        location.Children.Add(_directory);
        _storageSpace.FontFamily = UIFont;
        location.Children.Add(_storageSpace);
        storage.Children.Add(location);
        var locationActions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6, VerticalAlignment = VerticalAlignment.Center };
        ConfigureButton(_changeDirectory, "更改", "LocalModelsChangeDirectory");
        var openDirectory = ActionButton("打开文件夹", "LocalModelsOpenDirectory");
        locationActions.Children.Add(_changeDirectory);
        locationActions.Children.Add(openDirectory);
        Grid.SetColumn(locationActions, 1);
        storage.Children.Add(locationActions);
        card.Children.Add(storage);
        ConfigurePicker(_policy, "LocalModelsLoadPolicy", "模型加载策略", ("auto", "自动调度"),
            ("on-demand", "仅使用时加载"), ("preload", "启动后预热当前模型"));
        _policy.IsEnabled = false;
        var policyRow = new Grid { ColumnSpacing = 10 };
        policyRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        policyRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        policyRow.Children.Add(_policy);
        _hardware.FontFamily = UIFont;
        _hardware.VerticalAlignment = VerticalAlignment.Center;
        _hardware.MaxLines = 2;
        _hardware.TextTrimming = TextTrimming.CharacterEllipsis;
        Grid.SetColumn(_hardware, 1);
        policyRow.Children.Add(_hardware);
        card.Children.Add(policyRow);
        settings.Children.Add(new Border { Child = card, Padding = new Thickness(12), CornerRadius = new CornerRadius(12),
            Background = AppearanceService.GetBrush("KynxaSettingsCardBrush") });
        var toolbar = new Grid { ColumnSpacing = 8 };
        toolbar.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        toolbar.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        toolbar.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        _search.FontFamily = UIFont;
        _search.CornerRadius = new CornerRadius(8);
        UiLocalization.Bind(_search, TextBox.PlaceholderTextProperty, "搜索名称、系列或量化");
        UiLocalization.Bind(_search, AutomationProperties.NameProperty, "搜索本地模型");
        AutomationProperties.SetAutomationId(_search, "LocalModelsSearch");
        toolbar.Children.Add(_search);
        ConfigurePicker(_filter, "LocalModelsFilter", "模型筛选", ("all", "全部模型"), ("installed", "已安装"), ("downloading", "下载中"));
        _filter.SelectedIndex = 0;
        Grid.SetColumn(_filter, 1);
        toolbar.Children.Add(_filter);
        ConfigureButton(_import, "导入 GGUF", "LocalModelsImport");
        Grid.SetColumn(_import, 2);
        toolbar.Children.Add(_import);
        settings.Children.Add(toolbar);
        var details = new Grid();
        details.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        details.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        _count.FontFamily = UIFont;
        _count.VerticalAlignment = VerticalAlignment.Center;
        details.Children.Add(_count);
        var refresh = ActionButton("刷新", "LocalModelsRefresh");
        Grid.SetColumn(refresh, 1);
        details.Children.Add(refresh);
        settings.Children.Add(details);
        UiLocalization.Bind(_count, ToolTipService.ToolTipProperty, "离开页面不会取消下载；空闲驻留与唤醒由模型运行时和资源策略管理。");
        Grid.SetRow(settings, 1);
        _root.Children.Add(settings);

        _models.ItemsSource = _visibleRows;
        _models.FontFamily = UIFont;
        _models.Margin = new Thickness(14, 0, 14, 16);
        _models.ItemTemplate = (DataTemplate)XamlReader.Load(ModelTemplate);
        AutomationProperties.SetAutomationId(_models, "LocalModelsList");
        ScrollViewer.SetHorizontalScrollBarVisibility(_models, ScrollBarVisibility.Disabled);
        _models.ContainerContentChanging += BindModelActions;
        var listArea = new Grid();
        listArea.Children.Add(_models);
        UiLocalization.Bind(_empty, TextBlock.TextProperty, "没有匹配的模型。");
        _empty.FontFamily = UIFont;
        _empty.IsHitTestVisible = false;
        listArea.Children.Add(_empty);
        Grid.SetRow(listArea, 2);
        _root.Children.Add(listArea);
        Content = _root;
        _root.Loaded += async (_, _) =>
        {
            if (_loaded) return;
            _loaded = true;
            await RefreshAsync();
        };
        _enabled.Toggled += async (_, _) =>
        {
            if (!_rendering && _snapshot is not null)
            {
                bool enabled = _enabled.IsOn;
                await RunOperationAsync(() => _api.SettingsAsync(enabled: enabled, token: _lifetime.Token), settings: true);
            }
        };
        _policy.SelectionChanged += async (_, _) =>
        {
            if (!_rendering && _snapshot is not null && _policy.SelectedItem is ComboBoxItem { Tag: string policy })
                await RunOperationAsync(() => _api.SettingsAsync(loadPolicy: policy, token: _lifetime.Token), settings: true);
        };
        _search.TextChanged += (_, _) => ApplyFilter();
        _filter.SelectionChanged += (_, _) => ApplyFilter();
        _changeDirectory.Click += async (_, _) => await ChangeDirectoryAsync();
        openDirectory.Click += async (_, _) => await OpenDirectoryAsync();
        _import.Click += async (_, _) => await ImportModelAsync();
        refresh.Click += async (_, _) => await RefreshAsync();
    }

    private static FontFamily UIFont => (FontFamily)Application.Current.Resources["KynxaUIFont"];

    private void BindModelActions(ListViewBase sender, ContainerContentChangingEventArgs args)
    {
        if (args.InRecycleQueue) return;
        if (args.Phase == 0)
        {
            args.RegisterUpdateCallback(1, BindModelActions);
            return;
        }
        if (args.ItemContainer.ContentTemplateRoot is not FrameworkElement root) return;
        foreach (string name in new[] { "LocalModelInstall", "LocalModelPause", "LocalModelResume", "LocalModelCancel",
            "LocalModelEnable", "LocalModelDelete" })
        {
            if (root.FindName(name) is not Button button) continue;
            button.FocusVisualPrimaryBrush = AppearanceService.GetBrush("KynxaSecondaryTextBrush");
            button.Click -= ModelAction_Click;
            button.Click += ModelAction_Click;
        }
    }

    private static TextBlock Label(string key, double fontSize = 13, bool secondary = false)
    {
        var label = new TextBlock { FontFamily = UIFont, FontSize = fontSize, TextWrapping = TextWrapping.Wrap,
            Foreground = AppearanceService.GetBrush("KynxaTextBrush"), Opacity = secondary ? 0.65 : 1 };
        UiLocalization.Bind(label, TextBlock.TextProperty, key);
        return label;
    }

    private static Button ActionButton(string key, string id)
    {
        var button = new Button();
        ConfigureButton(button, key, id);
        return button;
    }

    private static void ConfigureButton(Button button, string key, string id)
    {
        button.Style = (Style)Application.Current.Resources["KynxaQuietButtonStyle"];
        button.FocusVisualPrimaryBrush = AppearanceService.GetBrush("KynxaSecondaryTextBrush");
        button.FontFamily = UIFont;
        button.CornerRadius = new CornerRadius(8);
        button.Padding = new Thickness(10, 6, 10, 6);
        UiLocalization.Bind(button, Button.ContentProperty, key);
        UiLocalization.Bind(button, AutomationProperties.NameProperty, key);
        AutomationProperties.SetAutomationId(button, id);
    }

    private static void ConfigurePicker(ComboBox picker, string id, string label, params (string Value, string Label)[] options)
    {
        picker.FontFamily = UIFont;
        picker.CornerRadius = new CornerRadius(8);
        picker.HorizontalAlignment = HorizontalAlignment.Stretch;
        UiLocalization.Bind(picker, AutomationProperties.NameProperty, label);
        AutomationProperties.SetAutomationId(picker, id);
        foreach (var option in options)
        {
            var item = new ComboBoxItem { Tag = option.Value };
            UiLocalization.Bind(item, ComboBoxItem.ContentProperty, option.Label);
            picker.Items.Add(item);
        }
    }

    private static void ApplyNeutralResources(ResourceDictionary resources)
    {
        var focus = AppearanceService.GetBrush("KynxaSecondaryTextBrush");
        resources["KynxaFocusBrush"] = focus;
        resources["SystemControlFocusVisualSecondaryBrush"] = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        foreach (string key in new[] { "AccentFillColorDefaultBrush", "AccentFillColorSecondaryBrush", "AccentFillColorTertiaryBrush",
            "SystemControlHighlightAccentBrush", "AccentTextFillColorPrimaryBrush", "AccentTextFillColorSecondaryBrush" })
            resources[key] = focus;
        foreach (string key in new[] { "SystemControlFocusVisualPrimaryBrush", "TextControlBorderBrushFocused",
            "ComboBoxBorderBrushFocused", "ComboBoxBackgroundBorderBrushFocused", "ListViewItemFocusBorderBrush",
            "ToggleSwitchFillOn", "ToggleSwitchFillOnPointerOver", "ToggleSwitchFillOnPressed", "ToggleSwitchStrokeOn",
            "ToggleSwitchStrokeOnPointerOver", "ToggleSwitchStrokeOnPressed", "CheckBoxCheckBackgroundFillChecked",
            "CheckBoxCheckBackgroundFillCheckedPointerOver", "CheckBoxCheckBackgroundFillCheckedPressed", "CheckBoxCheckBorderBrushChecked" })
            resources[key] = focus;
        // Keep dropdown selection under the shared accent and high-contrast palette.
        // 下拉选中沿用共享强调色及高对比度配色，避免页面局部中性色覆盖选中背景与标记。
        foreach (string key in new[] { "ListViewItemBackgroundSelected", "ListViewItemBackgroundSelectedPointerOver", "ListViewItemBackgroundSelectedPressed" })
            resources[key] = AppearanceService.GetBrush("KynxaControlHoverBrush");
    }

    private const string ModelTemplate = """
        <DataTemplate xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation">
          <Border Background="{ThemeResource KynxaSettingsCardBrush}" CornerRadius="12" Padding="12,8" Margin="4,3">
            <Grid ColumnSpacing="12">
              <Grid.ColumnDefinitions><ColumnDefinition Width="*"/><ColumnDefinition Width="Auto"/></Grid.ColumnDefinitions>
              <StackPanel Spacing="2">
                <Grid ColumnSpacing="8">
                  <Grid.ColumnDefinitions><ColumnDefinition Width="*"/><ColumnDefinition Width="Auto"/></Grid.ColumnDefinitions>
                  <TextBlock Text="{Binding Title}" ToolTipService.ToolTip="{Binding Title}" FontFamily="{StaticResource KynxaUIFont}" FontSize="14" FontWeight="SemiBold" TextTrimming="CharacterEllipsis"/>
                  <TextBlock Grid.Column="1" Text="{Binding State}" FontFamily="{StaticResource KynxaUIFont}" FontSize="11" VerticalAlignment="Center"/>
                </Grid>
                <TextBlock Text="{Binding Metadata}" ToolTipService.ToolTip="{Binding Metadata}" FontFamily="{StaticResource KynxaUIFont}" FontSize="12" Opacity="0.7" TextTrimming="CharacterEllipsis"/>
                <TextBlock Text="{Binding Requirements}" FontFamily="{StaticResource KynxaUIFont}" FontSize="11" Opacity="0.7" TextTrimming="CharacterEllipsis"/>
                <Grid ColumnSpacing="8">
                  <Grid.ColumnDefinitions><ColumnDefinition Width="*"/><ColumnDefinition Width="Auto"/></Grid.ColumnDefinitions>
                  <TextBlock Text="{Binding Description}" FontFamily="{StaticResource KynxaUIFont}" FontSize="11" Opacity="0.7" TextTrimming="CharacterEllipsis"/>
                  <TextBlock Grid.Column="1" Text="{Binding RecommendedLabel}" FontFamily="{StaticResource KynxaUIFont}" FontSize="11" Opacity="0.7"/>
                </Grid>
                <TextBlock Text="{Binding PathText}" Visibility="{Binding PathVisibility}" ToolTipService.ToolTip="{Binding PathText}" FontFamily="{StaticResource KynxaUIFont}" FontSize="11" Opacity="0.65" TextTrimming="CharacterEllipsis" IsTextSelectionEnabled="True"/>
                <StackPanel Spacing="2" Visibility="{Binding ProgressVisibility}">
                  <ProgressBar Value="{Binding Progress}" Minimum="0" Maximum="100" Height="3" Foreground="{ThemeResource KynxaFocusBrush}"/>
                  <TextBlock Text="{Binding ProgressText}" FontFamily="{StaticResource KynxaUIFont}" FontSize="11" Opacity="0.7"/>
                </StackPanel>
                <TextBlock Text="{Binding ErrorText}" Visibility="{Binding ErrorVisibility}" FontFamily="{StaticResource KynxaUIFont}" FontSize="12" TextWrapping="Wrap"/>
              </StackPanel>
              <StackPanel Grid.Column="1" Orientation="Horizontal" Spacing="4" VerticalAlignment="Center">
                <Button Name="LocalModelInstall" Tag="{Binding Id}" Content="{Binding InstallLabel}" Visibility="{Binding InstallVisibility}" IsEnabled="{Binding ActionsEnabled}" Style="{StaticResource KynxaQuietButtonStyle}" CornerRadius="8" FontSize="12" Padding="8,5"/>
                <Button Name="LocalModelPause" Tag="{Binding Id}" Content="{Binding PauseLabel}" Visibility="{Binding PauseVisibility}" IsEnabled="{Binding ActionsEnabled}" Style="{StaticResource KynxaQuietButtonStyle}" CornerRadius="8" FontSize="12" Padding="8,5"/>
                <Button Name="LocalModelResume" Tag="{Binding Id}" Content="{Binding ResumeLabel}" Visibility="{Binding ResumeVisibility}" IsEnabled="{Binding ActionsEnabled}" Style="{StaticResource KynxaQuietButtonStyle}" CornerRadius="8" FontSize="12" Padding="8,5"/>
                <Button Name="LocalModelCancel" Tag="{Binding Id}" Content="{Binding CancelLabel}" Visibility="{Binding CancelVisibility}" IsEnabled="{Binding ActionsEnabled}" Style="{StaticResource KynxaQuietButtonStyle}" CornerRadius="8" FontSize="12" Padding="8,5"/>
                <Button Name="LocalModelEnable" Tag="{Binding Id}" Content="{Binding EnableLabel}" Visibility="{Binding InstalledVisibility}" IsEnabled="{Binding EnableAllowed}" Style="{StaticResource KynxaQuietButtonStyle}" CornerRadius="8" FontSize="12" Padding="8,5"/>
                <Button Name="LocalModelDelete" Tag="{Binding Id}" Content="{Binding DeleteLabel}" Visibility="{Binding DeleteVisibility}" IsEnabled="{Binding ActionsEnabled}" Style="{StaticResource KynxaQuietButtonStyle}" CornerRadius="8" FontSize="12" Padding="8,5"/>
              </StackPanel>
            </Grid>
          </Border>
        </DataTemplate>
        """;
}
