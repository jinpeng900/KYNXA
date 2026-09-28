using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
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
        var menu = PickerMenu.Create(FlyoutPlacementMode.TopEdgeAlignedRight);
        var models = PickerMenu.CreateList("ModelPickerList", "模型列表");
        models.ItemsSource = _availableModels.Length > 0
            ? _availableModels.Cast<object>().ToArray() : new object[] { "尚无已配置模型" };
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
            DispatcherQueue.TryEnqueue(() => OpenModelManagement(customModels: true));
        };
        menu.Content = PickerMenu.WithFixedFooter(models, configure,
            Math.Max(120, Math.Min(360, XamlRoot.Size.Height - 32)));
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
        try { _modelSelectionStore.Save(choice); }
        catch { /* The in-memory selection remains usable. */ }
        menu.Hide();
    }
}
