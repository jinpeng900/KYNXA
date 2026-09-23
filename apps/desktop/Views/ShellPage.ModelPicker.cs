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
        var menu = new Flyout
        {
            Placement = FlyoutPlacementMode.TopEdgeAlignedRight,
            FlyoutPresenterStyle = (Style)Application.Current.Resources["KynxaModelFlyoutPresenterStyle"]
        };
        // The footer is a sibling of the scrolling list, so it never scrolls out of view.
        var content = new Grid { Height = Math.Max(120, Math.Min(360, XamlRoot.Size.Height - 32)) };
        content.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        content.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        content.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        var models = new ListView
        {
            ItemsSource = ModelPreviewNames, SelectedItem = _selectedModelName,
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
        models.ItemClick += (_, args) => SelectModel((string)args.ClickedItem, menu);
        models.KeyDown += (_, args) =>
        {
            if (args.Key is Windows.System.VirtualKey.Enter or Windows.System.VirtualKey.Space && models.SelectedItem is string name)
            {
                SelectModel(name, menu);
                args.Handled = true;
            }
        };
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
            Content = "配置自定义模型", Style = (Style)Application.Current.Resources["KynxaModelFooterButtonStyle"]
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
