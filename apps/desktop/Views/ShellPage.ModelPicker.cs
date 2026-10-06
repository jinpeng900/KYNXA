using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Markup;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private ModelSelectionStore _modelSelectionStore = new(StoragePaths.DesktopDirectory);
    private readonly ModelApiClient _modelApiClient = new();
    private ModelChoice? _selectedModel;
    private ModelChoice[] _availableModels = [];
    private CancellationTokenSource? _modelPickerRefresh;
    private bool _isModelCatalogLoading;
    private bool _hasLoadedModelCatalog;
    private bool _isModelPickerOpening;
    private string? _modelCatalogErrorKey;
    private Flyout? _modelPickerFlyout;
    private sealed record ModelPickerRow(ModelChoice Choice, bool IsCurrent)
    {
        public string ProviderId => Choice.ProviderId;
        public string ProviderName => Choice.ProviderName;
        public string Name => Choice.Name;
        public string ModelId => Choice.ModelId;
        public Visibility ModelIdVisibility => string.Equals(Name, ModelId, StringComparison.Ordinal)
            ? Visibility.Collapsed : Visibility.Visible;
        public Visibility CurrentVisibility => IsCurrent ? Visibility.Visible : Visibility.Collapsed;
        public double RowHeight => ModelIdVisibility == Visibility.Visible ? 72 : 54;
    }

    private void InitializeModelPicker()
    {
        try { _selectedModel = _modelSelectionStore.Load(); }
        catch { /* A preference must not prevent the composer from opening. 中文：偏好文件不能阻止输入区打开。 */ }
        UpdateModelPickerLabel();
        _ = RefreshModelPickerAsync();
    }

    private async Task<bool> RefreshModelPickerAsync()
    {
        if (_chatClosing) return false;
        CancelModelPickerRefresh();
        using var cancellation = new CancellationTokenSource();
        _modelPickerRefresh = cancellation;
        _isModelCatalogLoading = true;
        _modelCatalogErrorKey = null;
        UpdateModelPickerLabel();
        try
        {
            var providers = await _modelApiClient.ListAsync(cancellation.Token);
            if (_chatClosing || cancellation.IsCancellationRequested || !ReferenceEquals(_modelPickerRefresh, cancellation)) return false;
            _availableModels = providers.SelectMany(provider => provider.Models.Select(id =>
                new ModelChoice(provider.ProviderId, provider.DisplayName, id))).ToArray();
            _hasLoadedModelCatalog = true;
            if (_selectedModel is not null)
            {
                // Match stable identities after a connection rename; clear only after an authoritative successful read.
                // 连接改名后仍按稳定身份匹配；只有成功读取权威列表后才清除失效选择。
                var currentChoice = _availableModels.FirstOrDefault(choice =>
                    choice.ProviderId == _selectedModel.ProviderId && choice.ModelId == _selectedModel.ModelId);
                _selectedModel = currentChoice;
                if (currentChoice is null)
                {
                    try { _modelSelectionStore.Save(null); }
                    catch (Exception error) when (error is IOException or UnauthorizedAccessException)
                    { /* A UI preference failure must not invalidate the loaded catalog. 中文：UI 偏好写入失败不能使成功读取的列表失效。 */ }
                }
            }
            return true;
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested) { return false; }
        catch (Exception error)
        {
            if (_chatClosing || cancellation.IsCancellationRequested || !ReferenceEquals(_modelPickerRefresh, cancellation)) return false;
            // Keep the last successful catalog and selection; report failure without exposing connection details.
            // 保留最近成功读取的列表和选择；明确报告失败，不显示连接内部信息。
            _modelCatalogErrorKey = error is OperationCanceledException
                ? "读取模型连接超时，请重试。"
                : "无法读取模型连接，请检查本机模型服务后重试。";
            return false;
        }
        finally
        {
            if (ReferenceEquals(_modelPickerRefresh, cancellation))
            {
                _modelPickerRefresh = null;
                _isModelCatalogLoading = false;
                if (!_chatClosing) UpdateModelPickerLabel();
            }
        }
    }

    private void CancelModelPickerRefresh()
    {
        var pending = _modelPickerRefresh;
        _modelPickerRefresh = null;
        pending?.Cancel();
        _isModelCatalogLoading = false;
        if (!_chatClosing) UpdateModelStatusPresentation();
    }

    private void UpdateModelPickerLabel()
    {
        SelectedModelLabel.Text = _selectedModel?.Name ?? UiText.Get(_isModelCatalogLoading ? "读取模型连接中…" : "模型选择");
        string modelLabel = _selectedModel is null ? UiText.Get("模型选择")
            : $"{_selectedModel.Label}\n{_selectedModel.ModelId}";
        AutomationProperties.SetName(ModelPickerButton, _selectedModel?.Label ?? UiText.Get("模型选择"));
        ToolTipService.SetToolTip(ModelPickerButton, modelLabel);
        UpdateModelStatusPresentation();
    }

    private void UpdateModelStatusPresentation()
    {
        string statusKey;
        string brushKey = "KynxaSecondaryTextBrush";
        if (_isModelCatalogLoading) statusKey = "读取模型连接中…";
        else if (_modelCatalogErrorKey is not null)
        {
            statusKey = _modelCatalogErrorKey;
            brushKey = "KynxaPermissionFullBrush";
        }
        else if (!_hasLoadedModelCatalog) statusKey = "模型列表尚未读取，点击重新读取。";
        else if (_availableModels.Length == 0) statusKey = "尚无已配置模型，请先添加连接。";
        else if (_selectedModel is null) statusKey = "模型列表已读取，请选择模型。";
        else
        {
            statusKey = "已选择模型：{0}。此状态不代表模型调用已验证。";
            brushKey = "KynxaOnlineBrush";
        }
        string status = _selectedModel is not null && brushKey == "KynxaOnlineBrush"
            ? string.Format(UiText.Get(statusKey), _selectedModel.Label) : UiText.Get(statusKey);
        AutomationProperties.SetName(ModelStatusButton, status);
        AutomationProperties.SetHelpText(ModelPickerButton, status);
        ToolTipService.SetToolTip(ModelStatusButton, status);
        ModelStatusDot.Fill = (Brush)Application.Current.Resources[brushKey];
    }

    private void ModelStatusButton_Click(object sender, RoutedEventArgs e) => ModelPickerButton_Click(sender, e);

    private async void ModelPickerButton_Click(object sender, RoutedEventArgs e)
    {
        if (_isModelPickerOpening || _chatClosing) return;
        _isModelPickerOpening = true;
        try
        {
            _modelPickerFlyout?.Hide();
            bool loaded = await RefreshModelPickerAsync();
            if (_chatClosing || XamlRoot is null || (!loaded && _modelCatalogErrorKey is null)) return;
            ShowModelPicker(sender as FrameworkElement ?? ModelPickerButton);
        }
        finally { _isModelPickerOpening = false; }
    }

    private void ShowModelPicker(FrameworkElement anchor)
    {
        if (_chatClosing || XamlRoot is null) return;
        var menu = PickerMenu.Create(FlyoutPlacementMode.TopEdgeAlignedRight);
        _modelPickerFlyout = menu;
        menu.Closed += (_, _) =>
        {
            if (ReferenceEquals(_modelPickerFlyout, menu)) _modelPickerFlyout = null;
        };
        var models = PickerMenu.CreateList("ModelPickerList", UiText.Get("模型列表"));
        UiLocalization.Bind(models, AutomationProperties.NameProperty, "模型列表");
        models.ItemTemplate = (DataTemplate)XamlReader.Load("""
            <DataTemplate xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation">
                <Grid ColumnSpacing="8">
                    <Grid.ColumnDefinitions>
                        <ColumnDefinition Width="*" />
                        <ColumnDefinition Width="18" />
                    </Grid.ColumnDefinitions>
                    <StackPanel Spacing="3" Padding="0,4">
                        <TextBlock Text="{Binding Name}" ToolTipService.ToolTip="{Binding Name}" FontSize="13" TextTrimming="CharacterEllipsis" />
                        <TextBlock Text="{Binding ProviderName}" ToolTipService.ToolTip="{Binding ProviderName}" FontSize="11" Foreground="{StaticResource KynxaSecondaryTextBrush}" TextTrimming="CharacterEllipsis" />
                        <TextBlock Text="{Binding ModelId}" ToolTipService.ToolTip="{Binding ModelId}" Visibility="{Binding ModelIdVisibility}" FontSize="11" Foreground="{StaticResource KynxaSecondaryTextBrush}" TextTrimming="CharacterEllipsis" />
                    </StackPanel>
                    <FontIcon Grid.Column="1" Glyph="&#xE73E;" FontSize="12" Margin="0,7,0,0" VerticalAlignment="Top" IsHitTestVisible="False" Visibility="{Binding CurrentVisibility}" Foreground="{StaticResource KynxaSecondaryTextBrush}" />
                </Grid>
            </DataTemplate>
            """);
        var rowStyle = new Style(typeof(ListViewItem))
        {
            BasedOn = (Style)Application.Current.Resources["KynxaModelListItemStyle"]
        };
        rowStyle.Setters.Add(new Setter(FrameworkElement.HeightProperty, double.NaN));
        rowStyle.Setters.Add(new Setter(FrameworkElement.MinHeightProperty, 54d));
        rowStyle.Setters.Add(new Setter(Control.PaddingProperty, new Thickness(10, 4, 10, 4)));
        rowStyle.Setters.Add(new Setter(Control.HorizontalContentAlignmentProperty, HorizontalAlignment.Stretch));
        rowStyle.Setters.Add(new Setter(Control.CornerRadiusProperty, new CornerRadius(8)));
        models.ItemContainerStyle = rowStyle;
        models.ItemClick += (_, args) =>
        {
            if (args.ClickedItem is ModelPickerRow row) SelectModel(row.Choice, menu);
        };
        models.KeyDown += (_, args) =>
        {
            if (args.Key == Windows.System.VirtualKey.Escape)
            {
                menu.Hide();
                args.Handled = true;
            }
            else if (args.Key is Windows.System.VirtualKey.Enter or Windows.System.VirtualKey.Space &&
                models.SelectedItem is ModelPickerRow row)
            {
                SelectModel(row.Choice, menu);
                args.Handled = true;
            }
        };
        var configure = PickerMenu.Action(UiText.Get("配置模型连接"), "ConfigureCustomModelsButton");
        UiLocalization.Bind(configure, ContentControl.ContentProperty, "配置模型连接");
        UiLocalization.Bind(configure, AutomationProperties.NameProperty, "配置模型连接");
        configure.Click += (_, _) =>
        {
            menu.Hide();
            DispatcherQueue.TryEnqueue(() => OpenModelManagement(customModels: true));
        };
        var footer = new StackPanel();
        if (_modelCatalogErrorKey is not null)
        {
            var retry = PickerMenu.Action(UiText.Get("重新读取模型连接"), "RetryModelCatalogButton");
            UiLocalization.Bind(retry, ContentControl.ContentProperty, "重新读取模型连接");
            UiLocalization.Bind(retry, AutomationProperties.NameProperty, "重新读取模型连接");
            retry.Click += (_, _) =>
            {
                menu.Hide();
                ModelPickerButton_Click(anchor, new RoutedEventArgs());
            };
            footer.Children.Add(retry);
        }
        footer.Children.Add(configure);

        var body = new Grid();
        StackPanel? errorPanel = null;
        body.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        body.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        body.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        if (_modelCatalogErrorKey is not null)
        {
            errorPanel = new StackPanel { Margin = new Thickness(12, 8, 12, 4), Spacing = 4 };
            var errorText = new TextBlock
            {
                FontSize = 12, TextWrapping = TextWrapping.Wrap,
                Foreground = (Brush)Application.Current.Resources["KynxaSecondaryTextBrush"]
            };
            UiLocalization.Bind(errorText, TextBlock.TextProperty, _modelCatalogErrorKey);
            errorPanel.Children.Add(errorText);
            if (_availableModels.Length > 0)
            {
                var cacheText = new TextBlock
                {
                    FontSize = 12, TextWrapping = TextWrapping.Wrap,
                    Foreground = (Brush)Application.Current.Resources["KynxaSecondaryTextBrush"]
                };
                UiLocalization.Bind(cacheText, TextBlock.TextProperty, "下面保留上次读取的模型。");
                errorPanel.Children.Add(cacheText);
            }
            body.Children.Add(errorPanel);
        }
        var search = new TextBox
        {
            FontSize = 12, Margin = new Thickness(6, 4, 6, 8), MinHeight = 34
        };
        AutomationProperties.SetAutomationId(search, "ModelPickerSearchBox");
        UiLocalization.Bind(search, TextBox.PlaceholderTextProperty, "搜索模型或连接");
        UiLocalization.Bind(search, AutomationProperties.NameProperty, "搜索模型名称、ID或连接");
        UiLocalization.Bind(search, AutomationProperties.HelpTextProperty, "向下方向键进入结果，只有一个结果时可按 Enter 选择。");
        Grid.SetRow(search, 1);
        body.Children.Add(search);
        Grid.SetRow(models, 2);
        body.Children.Add(models);
        var empty = new TextBlock
        {
            Margin = new Thickness(12), FontSize = 13, TextWrapping = TextWrapping.Wrap,
            IsHitTestVisible = false, Foreground = (Brush)Application.Current.Resources["KynxaSecondaryTextBrush"]
        };
        AutomationProperties.SetLiveSetting(empty, Microsoft.UI.Xaml.Automation.Peers.AutomationLiveSetting.Polite);
        Grid.SetRow(empty, 2);
        body.Children.Add(empty);
        // The row is a presentation projection; selection and persistence still use the original stable choice.
        // 列表行只是展示投影；选择和保存仍使用原始稳定模型身份。
        var choices = _availableModels.Select(choice => new ModelPickerRow(choice,
            choice.ProviderId == _selectedModel?.ProviderId && choice.ModelId == _selectedModel?.ModelId)).ToArray();
        void FilterModels()
        {
            string query = search.Text.Trim();
            var filtered = choices.Where(choice => query.Length == 0 ||
                choice.Name.Contains(query, StringComparison.OrdinalIgnoreCase) ||
                choice.ModelId.Contains(query, StringComparison.OrdinalIgnoreCase) ||
                choice.ProviderName.Contains(query, StringComparison.OrdinalIgnoreCase) ||
                choice.ProviderId.Contains(query, StringComparison.OrdinalIgnoreCase)).ToArray();
            models.ItemsSource = filtered;
            models.SelectedItem = filtered.FirstOrDefault(choice =>
                choice.ProviderId == _selectedModel?.ProviderId && choice.ModelId == _selectedModel?.ModelId);
            string emptyKey = "没有匹配的模型，请换个关键词或清除搜索。";
            if (choices.Length == 0)
                emptyKey = _modelCatalogErrorKey is null ? "尚无已配置模型，请先添加连接。" : "模型列表暂时无法读取。";
            UiLocalization.Bind(empty, TextBlock.TextProperty, emptyKey);
            empty.Visibility = filtered.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
        }
        search.TextChanged += (_, _) => FilterModels();
        bool isSearchComposing = false, suppressSearchEnter = false;
        search.TextCompositionStarted += (_, _) => isSearchComposing = true;
        search.TextCompositionEnded += (_, _) => { isSearchComposing = false; suppressSearchEnter = true; };
        search.KeyUp += (_, _) => { if (!isSearchComposing) suppressSearchEnter = false; };
        search.KeyDown += (_, args) =>
        {
            if (isSearchComposing) return;
            if (args.Key == Windows.System.VirtualKey.Enter && suppressSearchEnter)
            {
                suppressSearchEnter = false;
                return;
            }
            if (args.Key != Windows.System.VirtualKey.Enter) suppressSearchEnter = false;
            if (args.Key == Windows.System.VirtualKey.Escape)
            {
                menu.Hide();
                args.Handled = true;
            }
            else if (models.Items.Count > 0 && args.Key == Windows.System.VirtualKey.Down)
            {
                models.SelectedItem ??= models.Items[0];
                models.Focus(FocusState.Keyboard);
                models.ScrollIntoView(models.SelectedItem);
                args.Handled = true;
            }
            else if (models.Items.Count == 1 && args.Key == Windows.System.VirtualKey.Enter &&
                models.Items[0] is ModelPickerRow row)
            {
                SelectModel(row.Choice, menu);
                args.Handled = true;
            }
        };
        FilterModels();
        double menuWidth = PickerMenu.SetContentWidth(menu, 340, XamlRoot.Size.Width);
        var measureSize = new Windows.Foundation.Size(menuWidth, double.PositiveInfinity);
        search.Measure(measureSize);
        footer.Measure(measureSize);
        errorPanel?.Measure(measureSize);
        double rowsHeight = choices.Length == 0 ? 64 : choices.Sum(row => row.RowHeight);
        double preferredHeight = search.DesiredSize.Height + footer.DesiredSize.Height + 9
            + (errorPanel?.DesiredSize.Height ?? 0) + rowsHeight;
        // Size once from the initial catalog. Filtering keeps the footer and popup anchor steady.
        // 仅根据首次打开的列表确定高度；筛选时保持底部操作和弹出位置稳定。
        double menuHeight = Math.Min(Math.Min(400, Math.Max(120, XamlRoot.Size.Height - 32)), preferredHeight);
        var menuContent = PickerMenu.WithFixedFooter(body, footer, menuHeight);
        menuContent.Width = menuWidth;
        menu.Content = menuContent;
        menu.Opened += (_, _) =>
        {
            if (models.SelectedItem is not null) models.ScrollIntoView(models.SelectedItem);
            search.Focus(FocusState.Programmatic);
        };
        menu.ShowAt(anchor);
    }

    private void SelectModel(ModelChoice choice, Flyout menu)
    {
        if (_chatClosing) return;
        if (!_availableModels.Any(available => available.ProviderId == choice.ProviderId && available.ModelId == choice.ModelId)) return;
        CancelModelPickerRefresh();
        _selectedModel = choice;
        UpdateModelPickerLabel();
        try { _modelSelectionStore.Save(choice); }
        catch { /* The in-memory selection remains usable. 中文：内存中的选择仍可使用。 */ }
        menu.Hide();
    }
}
