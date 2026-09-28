using KYNXA_Desktop.Services;
using KYNXA_Desktop.Controls;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Windows.Storage;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private readonly ModelSelectionStore _modelSelectionStore = new(ApplicationData.Current.LocalFolder.Path);
    // Front-end preview names from the supplied reference. Runtime model discovery is not connected yet.
    private static readonly string[] ModelPreviewNames =
    [
        "GLM-5.1", "GLM-5v-Turbo", "MiniMax-M3", "Kimi-K3", "Kimi-K2.8-Preview",
        "Kimi-K2.7-Code", "Kimi-K2.6", "Deepseek-V4.1-Flash", "Deepseek-V4-Pro",
        "GPT-5", "GPT-4.1", "GPT-4.1-mini", "GPT-4o", "Claude Sonnet 4", "Claude Opus 4",
        "Gemini 2.5 Pro", "Gemini 2.5 Flash", "DeepSeek Chat", "DeepSeek Reasoner",
        "Qwen3 8B", "Qwen3 32B", "Qwen3 235B-A22B", "DeepSeek R1 7B", "Llama 3.2 3B", "Llama 3.3 70B"
    ];
    private string? _selectedModelName;

    private void InitializeModelPicker()
    {
        try
        {
            string? saved = _modelSelectionStore.Load();
            _selectedModelName = ModelPreviewNames.Contains(saved) ? saved : null;
        }
        catch { /* A UI preference must not prevent the composer from opening. */ }
        UpdateModelPickerLabel();
    }

    private void UpdateModelPickerLabel()
    {
        SelectedModelLabel.Text = _selectedModelName ?? "模型选择";
        AutomationProperties.SetName(ModelPickerButton, SelectedModelLabel.Text);
        ToolTipService.SetToolTip(ModelPickerButton, SelectedModelLabel.Text);
    }

    private void ModelPickerButton_Click(object sender, RoutedEventArgs e)
    {
        var menu = PickerMenu.Create(FlyoutPlacementMode.TopEdgeAlignedRight);
        var models = PickerMenu.CreateList("ModelPickerList", "模型列表");
        models.ItemsSource = ModelPreviewNames;
        models.SelectedItem = _selectedModelName;
        models.ItemClick += (_, args) => SelectModel((string)args.ClickedItem, menu);
        models.KeyDown += (_, args) =>
        {
            if (args.Key is Windows.System.VirtualKey.Enter or Windows.System.VirtualKey.Space && models.SelectedItem is string name)
            {
                SelectModel(name, menu);
                args.Handled = true;
            }
        };
        var configure = PickerMenu.Action("配置自定义模型", "ConfigureCustomModelsButton");
        configure.Click += (_, _) =>
        {
            menu.Hide();
            DispatcherQueue.TryEnqueue(() => OpenModelManagement(customModels: true));
        };
        menu.Content = PickerMenu.WithFixedFooter(models, configure, Math.Max(120, Math.Min(360, XamlRoot.Size.Height - 32)));
        menu.Opened += (_, _) =>
        {
            if (_selectedModelName is not null) models.ScrollIntoView(_selectedModelName);
        };
        menu.ShowAt(ModelPickerButton);
    }

    private void SelectModel(string name, Flyout menu)
    {
        _selectedModelName = name;
        UpdateModelPickerLabel();
        try { _modelSelectionStore.Save(name); }
        catch { /* The in-memory selection remains usable if settings cannot be written. */ }
        menu.Hide();
    }
}
