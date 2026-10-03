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
    private ModelProvider[] _modelProviders = [];
    private bool _modelsLoading;
    private bool _modelsLoaded;
    private string? _modelLoadError;
    private Task? _modelRefreshTask;
    private bool _modelPickerOpening;

    private void InitializeModelPicker()
    {
        try { _selectedModel = _modelSelectionStore.Load(); }
        catch { /* A preference must not prevent the composer from opening. */ }
        UpdateModelPickerLabel();
        _ = RefreshModelPickerAsync();
    }

    private Task RefreshModelPickerAsync() => _modelRefreshTask is { IsCompleted: false }
        ? _modelRefreshTask : _modelRefreshTask = RefreshModelPickerCoreAsync();

    private async Task RefreshModelPickerCoreAsync()
    {
        _modelsLoading = true;
        _modelLoadError = null;
        UpdateModelStatusPresentation();
        try
        {
            var providers = await _modelApiClient.ListAsync();
            if (_chatClosing) return;
            _modelProviders = providers;
            _availableModels = providers.SelectMany(provider => provider.Models.Select(id =>
                new ModelChoice(provider.ProviderId, provider.DisplayName, id))).ToArray();
            _modelsLoaded = true;
            if (_selectedModel is not null && !_availableModels.Any(choice =>
                choice.ProviderId == _selectedModel.ProviderId && choice.ModelId == _selectedModel.ModelId))
            {
                _selectedModel = null;
                try { _modelSelectionStore.Save(null); }
                catch (Exception error) when (error is IOException or UnauthorizedAccessException)
                { /* Clearing a stale preference must not hide a successfully loaded catalog. */ }
            }
        }
        catch (Exception error)
        {
            // Keep the last successful catalog and selection when the gateway is temporarily unavailable.
            _modelLoadError = error is InvalidOperationException ? error.Message
                : error is OperationCanceledException ? "读取模型连接超时，请重试。"
                : "无法读取模型连接，请检查本机模型服务后重试。";
        }
        finally
        {
            _modelsLoading = false;
            if (!_chatClosing)
            {
                UpdateModelPickerLabel();
                UpdateModelStatusPresentation();
            }
        }
    }

    private bool EnsureModelReadyForSend()
    {
        if (_modelsLoading)
        {
            UpdateModelStatusPresentation();
            ModelPickerButton.Focus(FocusState.Programmatic);
            return false;
        }
        if (_modelsLoaded && _modelLoadError is null && _selectedModel is not null && _availableModels.Any(choice =>
            choice.ProviderId == _selectedModel.ProviderId && choice.ModelId == _selectedModel.ModelId)) return true;

        // The caller checks this before saving a user message or clearing the input draft.
        UpdateModelStatusPresentation();
        if (_modelsLoaded && _availableModels.Length == 0 && _modelLoadError is null) OpenModelManagement();
        else ModelPickerButton_Click(ModelPickerButton, new RoutedEventArgs());
        return false;
    }

    private void UpdateModelPickerLabel()
    {
        SelectedModelLabel.Text = _selectedModel?.Name ?? "模型选择";
        AutomationProperties.SetName(ModelPickerButton, _selectedModel?.Label ?? "模型选择");
        ToolTipService.SetToolTip(ModelPickerButton, _selectedModel?.Label ?? "模型选择");
    }

    private async void ModelPickerButton_Click(object sender, RoutedEventArgs e)
    {
        if (_modelPickerOpening) return;
        _modelPickerOpening = true;
        try
        {
            await RefreshModelPickerAsync();
            if (!_chatClosing) ShowModelPicker();
        }
        finally { _modelPickerOpening = false; }
    }

    private void ShowModelPicker()
    {
        var menu = PickerMenu.Create(FlyoutPlacementMode.TopEdgeAlignedRight);
        var models = PickerMenu.CreateList("ModelPickerList", "模型列表");
        models.ItemTemplate = (DataTemplate)XamlReader.Load("""
            <DataTemplate xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation">
                <StackPanel Spacing="3" Padding="0,5">
                    <TextBlock Text="{Binding Name}" FontSize="13" TextTrimming="CharacterEllipsis" />
                    <TextBlock Text="{Binding ProviderName}" FontSize="11" Foreground="#888888" TextTrimming="CharacterEllipsis" />
                    <TextBlock Text="{Binding ModelId}" FontSize="11" Foreground="#888888" TextTrimming="CharacterEllipsis" />
                </StackPanel>
            </DataTemplate>
            """);
        var rowStyle = new Style(typeof(ListViewItem));
        rowStyle.Setters.Add(new Setter(FrameworkElement.MinHeightProperty, 72d));
        rowStyle.Setters.Add(new Setter(Control.PaddingProperty, new Thickness(10, 4, 10, 4)));
        rowStyle.Setters.Add(new Setter(Control.HorizontalContentAlignmentProperty, HorizontalAlignment.Stretch));
        rowStyle.Setters.Add(new Setter(Control.CornerRadiusProperty, new CornerRadius(8)));
        models.ItemContainerStyle = rowStyle;
        models.ItemsSource = _availableModels;
        models.SelectedItem = _availableModels.FirstOrDefault(choice =>
            choice.ProviderId == _selectedModel?.ProviderId && choice.ModelId == _selectedModel?.ModelId);
        models.ItemClick += (_, args) =>
        {
            if (args.ClickedItem is ModelChoice choice) SelectModel(choice, menu);
        };
        models.KeyDown += (_, args) =>
        {
            if (args.Key is Windows.System.VirtualKey.Enter or Windows.System.VirtualKey.Space &&
                models.SelectedItem is ModelChoice choice)
            {
                SelectModel(choice, menu);
                args.Handled = true;
            }
        };
        var configure = PickerMenu.Action("配置模型连接", "ConfigureCustomModelsButton");
        configure.Click += (_, _) =>
        {
            menu.Hide();
            DispatcherQueue.TryEnqueue(() => OpenModelManagement());
        };
        var footer = new StackPanel();
        if (_modelLoadError is not null)
        {
            var retry = PickerMenu.Action("重新读取模型连接", "RetryModelCatalogButton");
            retry.Click += (_, _) =>
            {
                menu.Hide();
                ModelPickerButton_Click(ModelPickerButton, new RoutedEventArgs());
            };
            footer.Children.Add(retry);
        }
        footer.Children.Add(configure);
        var body = new Grid();
        body.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        body.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        if (_modelLoadError is not null)
        {
            body.Children.Add(new TextBlock
            {
                Text = _modelLoadError + (_availableModels.Length > 0 ? "\n下面保留上次读取的模型。" : ""),
                Margin = new Thickness(12, 8, 12, 8), FontSize = 12, TextWrapping = TextWrapping.Wrap,
                Foreground = (Brush)Application.Current.Resources["KynxaSecondaryTextBrush"]
            });
        }
        Grid.SetRow(models, 1);
        body.Children.Add(models);
        if (_availableModels.Length == 0)
        {
            var empty = new TextBlock
            {
                Text = _modelLoadError is not null ? "模型列表暂时无法读取。" : "尚无已配置模型，请先添加连接。",
                Margin = new Thickness(12), FontSize = 13, TextWrapping = TextWrapping.Wrap
            };
            Grid.SetRow(empty, 1);
            body.Children.Add(empty);
        }
        var menuContent = PickerMenu.WithFixedFooter(body, footer,
            Math.Max(180, Math.Min(360, XamlRoot.Size.Height - 32)));
        menuContent.Width = Math.Min(340, Math.Max(240, XamlRoot.Size.Width - 32));
        menu.Content = menuContent;
        menu.Opened += (_, _) =>
        {
            if (models.SelectedItem is not null) models.ScrollIntoView(models.SelectedItem);
        };
        menu.ShowAt(ModelPickerButton);
    }

    private void SelectModel(ModelChoice choice, Flyout menu)
    {
        _selectedModel = choice;
        UpdateModelPickerLabel();
        UpdateModelStatusPresentation();
        try { _modelSelectionStore.Save(choice); }
        catch { /* The in-memory selection remains usable. */ }
        menu.Hide();
    }
}
