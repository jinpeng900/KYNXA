using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Markup;
using Windows.Storage;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private ModelSelectionStore _modelSelectionStore = new(StoragePaths.DesktopDirectory);
    private readonly ModelApiClient _modelApiClient = new();
    private ModelChoice? _selectedModel;
    private ModelChoice[] _availableModels = [];
    private CancellationTokenSource? _modelPickerRefresh;

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
        try
        {
            var providers = await _modelApiClient.ListAsync(cancellation.Token);
            if (_chatClosing || cancellation.IsCancellationRequested || !ReferenceEquals(_modelPickerRefresh, cancellation)) return false;
            _availableModels = providers.SelectMany(provider => provider.Models.Select(id =>
                new ModelChoice(provider.ProviderId, provider.DisplayName, id))).ToArray();
            if (_selectedModel is not null && !_availableModels.Any(choice =>
                choice.ProviderId == _selectedModel.ProviderId && choice.ModelId == _selectedModel.ModelId))
            {
                _selectedModel = null;
                _modelSelectionStore.Save(null);
            }
            UpdateModelPickerLabel();
            return true;
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested) { return false; }
        catch (Exception)
        {
            if (_chatClosing || cancellation.IsCancellationRequested || !ReferenceEquals(_modelPickerRefresh, cancellation)) return false;
            _availableModels = [];
            UpdateModelPickerLabel();
            return true;
        }
        finally
        {
            if (ReferenceEquals(_modelPickerRefresh, cancellation)) _modelPickerRefresh = null;
        }
    }

    private void CancelModelPickerRefresh()
    {
        var pending = _modelPickerRefresh;
        _modelPickerRefresh = null;
        pending?.Cancel();
    }

    private void UpdateModelPickerLabel()
    {
        SelectedModelLabel.Text = _selectedModel?.Name ?? UiText.Get("模型选择");
        AutomationProperties.SetName(ModelPickerButton, _selectedModel?.Label ?? UiText.Get("模型选择"));
        ToolTipService.SetToolTip(ModelPickerButton, _selectedModel?.Label ?? UiText.Get("模型选择"));
    }

    private async void ModelPickerButton_Click(object sender, RoutedEventArgs e)
    {
        if (!await RefreshModelPickerAsync() || _chatClosing || XamlRoot is null) return;
        var menu = PickerMenu.Create(FlyoutPlacementMode.TopEdgeAlignedRight);
        var models = PickerMenu.CreateList("ModelPickerList", UiText.Get("模型列表"));
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
        var configure = PickerMenu.Action(UiText.Get("配置模型连接"), "ConfigureCustomModelsButton");
        configure.Click += (_, _) =>
        {
            menu.Hide();
            DispatcherQueue.TryEnqueue(() => OpenModelManagement(customModels: true));
        };
        var body = new Grid();
        body.Children.Add(models);
        if (_availableModels.Length == 0) body.Children.Add(new TextBlock { Text = UiText.Get("尚无已配置模型"), Margin = new Thickness(12), FontSize = 13 });
        var menuContent = PickerMenu.WithFixedFooter(body, configure,
            Math.Max(120, Math.Min(360, XamlRoot.Size.Height - 32)));
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
        if (_chatClosing) return;
        CancelModelPickerRefresh();
        _selectedModel = choice;
        UpdateModelPickerLabel();
        try { _modelSelectionStore.Save(choice); }
        catch { /* The in-memory selection remains usable. 中文：内存中的选择仍可使用。 */ }
        menu.Hide();
    }
}
