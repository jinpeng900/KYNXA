using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using Windows.Storage;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private readonly ModelSelectionStore _modelSelectionStore = new(ApplicationData.Current.LocalFolder.Path);
    private readonly ModelApiClient _modelApiClient = new();
    private ModelChoice? _selectedModel;
    private ModelChoice[] _availableModels = [];

    private void InitializeModelPicker()
    {
        try { _selectedModel = _modelSelectionStore.Load(); }
        catch { /* A preference must not prevent the composer from opening. */ }
        UpdateModelPickerLabel();
        _ = RefreshModelPickerAsync();
    }

    private async Task RefreshModelPickerAsync()
    {
        try
        {
            var providers = await _modelApiClient.ListAsync();
            _availableModels = providers.SelectMany(provider => provider.Models.Select(id =>
                new ModelChoice(provider.ProviderId, provider.DisplayName, id))).ToArray();
            if (_selectedModel is not null && !_availableModels.Any(choice =>
                choice.ProviderId == _selectedModel.ProviderId && choice.ModelId == _selectedModel.ModelId))
            {
                _selectedModel = null;
                _modelSelectionStore.Save(null);
            }
        }
        catch (Exception) { _availableModels = []; }
        UpdateModelPickerLabel();
    }

    private void UpdateModelPickerLabel()
    {
        SelectedModelLabel.Text = _selectedModel?.ModelId ?? "模型选择";
        AutomationProperties.SetName(ModelPickerButton, _selectedModel?.Label ?? "模型选择");
        ToolTipService.SetToolTip(ModelPickerButton, _selectedModel?.Label ?? "模型选择");
    }

    private async void ModelPickerButton_Click(object sender, RoutedEventArgs e)
    {
        await RefreshModelPickerAsync();
        var menu = new Flyout
        {
            Placement = FlyoutPlacementMode.TopEdgeAlignedRight,
            FlyoutPresenterStyle = (Style)Application.Current.Resources["KynxaModelFlyoutPresenterStyle"]
        };
        var content = new Grid { Height = Math.Max(120, Math.Min(360, XamlRoot.Size.Height - 32)) };
        content.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        content.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        content.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        var models = new ListView
        {
            ItemsSource = _availableModels.Length > 0 ? _availableModels.Cast<object>().ToArray() : new object[] { "尚无已配置模型" },
            SelectedItem = _availableModels.FirstOrDefault(choice => choice.ProviderId == _selectedModel?.ProviderId && choice.ModelId == _selectedModel?.ModelId),
            IsItemClickEnabled = true, SelectionMode = ListViewSelectionMode.Single,
            ItemContainerStyle = (Style)Application.Current.Resources["KynxaModelListItemStyle"],
            MinHeight = 0, Padding = new Thickness(0)
        };
        models.Resources["ListViewItemSelectionIndicatorVisualEnabled"] = false;
        ScrollViewer.SetHorizontalScrollMode(models, ScrollMode.Disabled);
        ScrollViewer.SetHorizontalScrollBarVisibility(models, ScrollBarVisibility.Disabled);
        ScrollViewer.SetVerticalScrollMode(models, ScrollMode.Enabled);
        ScrollViewer.SetVerticalScrollBarVisibility(models, ScrollBarVisibility.Auto);
        AutomationProperties.SetAutomationId(models, "ModelPickerList");
        AutomationProperties.SetName(models, "模型列表");
        models.ItemClick += (_, args) => { if (args.ClickedItem is ModelChoice choice) SelectModel(choice, menu); };
        content.Children.Add(models);
        var separator = new Border
        {
            Height = 1, Margin = new Thickness(6, 4, 6, 4),
            Background = (Brush)Application.Current.Resources["KynxaDividerBrush"]
        };
        Grid.SetRow(separator, 1);
        content.Children.Add(separator);
        var configure = new Button
        {
            Content = "配置模型连接", Style = (Style)Application.Current.Resources["KynxaModelFooterButtonStyle"]
        };
        AutomationProperties.SetAutomationId(configure, "ConfigureCustomModelsButton");
        configure.Click += (_, _) =>
        {
            menu.Hide();
            DispatcherQueue.TryEnqueue(() => OpenModelManagement(customModels: true));
        };
        Grid.SetRow(configure, 2);
        content.Children.Add(configure);
        menu.Content = content;
        menu.ShowAt(ModelPickerButton);
    }

    private void SelectModel(ModelChoice choice, Flyout menu)
    {
        _selectedModel = choice;
        UpdateModelPickerLabel();
        try { _modelSelectionStore.Save(choice); }
        catch { /* The in-memory selection remains usable. */ }
        menu.Hide();
    }
}
