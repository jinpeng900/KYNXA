using KYNXA_Desktop.Services;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Graphics;

namespace KYNXA_Desktop;

public sealed partial class ModelManagementWindow : Window
{
    private readonly ModelApiClient _api = new();
    private ModelProvider[] _providers = [];

    public ModelManagementWindow()
    {
        InitializeComponent();
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(ModelTitleBar);
        AppWindow.SetIcon("Assets/AppIcon.ico");
        if (AppWindowTitleBar.IsCustomizationSupported())
        {
            AppWindow.TitleBar.ButtonBackgroundColor = Microsoft.UI.Colors.Transparent;
            AppWindow.TitleBar.ButtonInactiveBackgroundColor = Microsoft.UI.Colors.Transparent;
        }
        SizeAndCenterWindow();
        Closed += (_, _) => _api.Dispose();
        _ = RefreshAsync();
    }

    private void SizeAndCenterWindow()
    {
        DisplayArea area = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Primary);
        RectInt32 workArea = area.WorkArea;
        int width = Math.Min(1000, Math.Max(720, (int)(workArea.Width * 0.88)));
        int height = Math.Min(680, Math.Max(520, (int)(workArea.Height * 0.84)));
        AppWindow.MoveAndResize(new RectInt32(workArea.X + (workArea.Width - width) / 2,
            workArea.Y + (workArea.Height - height) / 2, width, height));
    }

    public void ShowCustomModels()
    {
        ClearForm();
        NameBox.Text = "自定义 API";
        ProviderIdBox.Text = "custom-api";
        StatusText.Text = "填写服务商提供的 OpenAI 兼容 Base URL、API Key 和 Model ID。";
    }

    private async Task RefreshAsync()
    {
        try
        {
            _providers = await _api.ListAsync();
            ProviderList.Children.Clear();
            foreach (var provider in _providers)
            {
                var button = new Button
                {
                    Content = $"{provider.DisplayName}  ·  {provider.Models.Length} 个模型",
                    HorizontalAlignment = HorizontalAlignment.Stretch,
                    HorizontalContentAlignment = HorizontalAlignment.Left,
                    Padding = new Thickness(12, 10, 12, 10),
                    Tag = provider
                };
                button.Click += (_, _) => SelectProvider(provider);
                ProviderList.Children.Add(button);
            }
            StatusText.Text = _providers.Length == 0 ? "尚无连接。可选择 Ollama 预设或填写自定义 API。" :
                $"已加载 {_providers.Length} 个连接。";
        }
        catch (Exception error) { StatusText.Text = $"模型服务不可用：{error.Message}。请先启动 apps/model-gateway。"; }
    }

    private void SelectProvider(ModelProvider provider)
    {
        NameBox.Text = provider.DisplayName;
        ProviderIdBox.Text = provider.ProviderId;
        BaseUrlBox.Text = provider.BaseUrl;
        ModelsBox.Text = string.Join(Environment.NewLine, provider.Models);
        ApiKeyBox.Password = string.Empty;
        StatusText.Text = provider.HasApiKey ? "API Key 已保存；留空可保留原 Key。" : "此连接尚未保存 API Key。";
    }

    private void NewConnectionButton_Click(object sender, RoutedEventArgs e)
        => ClearForm();

    private void ClearForm()
    {
        NameBox.Text = ProviderIdBox.Text = BaseUrlBox.Text = ModelsBox.Text = ApiKeyBox.Password = string.Empty;
        StatusText.Text = "填写连接信息后保存。";
    }

    private void LocalPresetButton_Click(object sender, RoutedEventArgs e)
    {
        ClearForm();
        NameBox.Text = "Ollama";
        ProviderIdBox.Text = "ollama";
        BaseUrlBox.Text = "http://127.0.0.1:11434/v1";
        StatusText.Text = "可点“测试并获取模型”；Ollama 默认无需 API Key。";
    }

    private void CustomPresetButton_Click(object sender, RoutedEventArgs e)
    {
        ShowCustomModels();
    }

    private ModelConnection Form() => new(ProviderIdBox.Text.Trim(), NameBox.Text.Trim(), BaseUrlBox.Text.Trim(),
        ModelsBox.Text.Split(['\r', '\n', ','], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries),
        string.IsNullOrEmpty(ApiKeyBox.Password) ? null : ApiKeyBox.Password);

    private async void TestConnectionButton_Click(object sender, RoutedEventArgs e)
    {
        ProbeButton.IsEnabled = false;
        StatusText.Text = "正在测试模型列表接口…";
        try
        {
            var result = await _api.TestAsync(Form());
            if (result.Models.Length > 0) ModelsBox.Text = string.Join(Environment.NewLine, result.Models);
            StatusText.Text = result.Models.Length > 0
                ? $"连接成功，{result.LatencyMs} ms；发现 {result.Models.Length} 个模型。"
                : $"连接成功，{result.LatencyMs} ms；未返回模型，请手动填写 Model ID。";
        }
        catch (Exception error) { StatusText.Text = $"测试失败：{error.Message}。若接口不提供 /models，可手动填 Model ID 后保存。"; }
        finally { ProbeButton.IsEnabled = true; }
    }

    private async void SaveConnectionButton_Click(object sender, RoutedEventArgs e)
    {
        SaveButton.IsEnabled = false;
        try
        {
            var provider = await _api.SaveAsync(Form());
            ApiKeyBox.Password = string.Empty;
            await RefreshAsync();
            StatusText.Text = $"已保存 {provider.DisplayName}。返回聊天页即可选择模型。";
        }
        catch (Exception error) { StatusText.Text = $"保存失败：{error.Message}"; }
        finally { SaveButton.IsEnabled = true; }
    }
}
