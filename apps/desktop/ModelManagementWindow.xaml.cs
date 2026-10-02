using KYNXA_Desktop.Services;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using System.Globalization;
using System.Net.Http;
using System.Text.RegularExpressions;
using Windows.Graphics;

namespace KYNXA_Desktop;

public sealed partial class ModelManagementWindow : Window
{
    private readonly ModelApiClient _api = new();
    private readonly CancellationTokenSource _lifetime = new();
    private ModelProvider[] _providers = [];
    private ModelProvider? _editing;
    private bool _changingPreset;
    private bool _closed;
    private bool _loaded;
    private bool _updatingModelList;
    private string[] _discoveredModels = [];

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
        Closed += (_, _) => { _closed = true; _lifetime.Cancel(); _api.Dispose(); };
        PresetBox.ItemsSource = ModelPresets.All;
        ApplyPreset(ModelPresets.All[0]);
        _ = InitializeAsync();
    }

    private void SizeAndCenterWindow()
    {
        var area = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Primary).WorkArea;
        int width = Math.Min(1120, (int)(area.Width * 0.92));
        int height = Math.Min(860, (int)(area.Height * 0.92));
        AppWindow.MoveAndResize(new RectInt32(area.X + (area.Width - width) / 2,
            area.Y + (area.Height - height) / 2, width, height));
    }

    // The composer opens the same preset-first setup page.
    public void ShowCustomModels() => ApplyPreset(ModelPresets.All[0]);

    private async Task InitializeAsync()
    {
        SetBusy(true);
        try
        {
            await RefreshAsync();
            // Resolve a new ID only after saved connections have been loaded.
            if (_editing is null && PresetBox.SelectedItem is ModelPreset preset)
                ProviderIdBox.Text = NewId(preset.Id);
        }
        catch (Exception error) { SetStatus(FriendlyError(error), true); }
        finally { if (!_closed) SetBusy(false); }
    }

    private async Task RefreshAsync()
    {
        var providers = await _api.ListAsync(_lifetime.Token);
        if (_closed) return;
        _providers = providers;
        _loaded = true;
        RenderProviders();
    }

    private void RenderProviders()
    {
        ProviderList.Children.Clear();
        EmptyConnections.Visibility = _providers.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
        ConnectionCount.Text = _providers.Length == 0 ? "配置后即可在聊天中选择" : $"{_providers.Length} 个已保存连接";
        foreach (var provider in _providers)
        {
            var content = new StackPanel { Spacing = 5 };
            content.Children.Add(new TextBlock { Text = provider.DisplayName, FontSize = 13,
                TextTrimming = TextTrimming.CharacterEllipsis, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
            content.Children.Add(new TextBlock { Text = $"{provider.Models.Length} 个模型", FontSize = 11,
                Foreground = (Brush)Application.Current.Resources["KynxaSecondaryTextBrush"] });
            var button = new Button
            {
                Content = content, HorizontalAlignment = HorizontalAlignment.Stretch,
                HorizontalContentAlignment = HorizontalAlignment.Stretch, Padding = new Thickness(12, 11, 12, 11),
                Background = _editing?.ProviderId == provider.ProviderId
                    ? new SolidColorBrush(Windows.UI.Color.FromArgb(255, 235, 234, 233))
                    : new SolidColorBrush(Microsoft.UI.Colors.Transparent)
            };
            AutomationProperties.SetName(button, $"{provider.DisplayName}，{provider.Models.Length} 个模型");
            button.Click += (_, _) => SelectProvider(provider);
            ProviderList.Children.Add(button);
        }
    }

    private string NewId(string prefix) => ModelPresets.UniqueId(prefix == "custom" ? "custom-api" : prefix,
        _providers.Select(p => p.ProviderId));

    private void ApplyPreset(ModelPreset preset)
    {
        _changingPreset = true;
        PresetBox.SelectedItem = preset;
        _changingPreset = false;
        _editing = null;
        _discoveredModels = [];
        ModelSearchBox.Text = "";
        SetProtocol(preset.Protocol);
        SetContextWindow(ModelApiClient.DefaultContextWindowTokens);
        EditorTitle.Text = "添加模型连接";
        NameBox.Text = preset.Id == "custom" ? "" : preset.Name;
        ProviderIdBox.IsReadOnly = false;
        ProviderIdBox.Text = NewId(preset.Id);
        BaseUrlBox.Text = preset.BaseUrl;
        ModelsBox.Text = string.Join(Environment.NewLine, preset.Models);
        ApiKeyBox.Password = "";
        UpdateEndpointHints();
        PresetHint.Text = preset.Hint;
        AdvancedSettings.IsExpanded = preset.Id == "custom";
        EditorScroll.ChangeView(null, 0, null);
        SetStatus(preset.Hint);
        RenderProviders();
        RenderModelOptions();
    }

    private void SelectProvider(ModelProvider provider)
    {
        _editing = provider;
        _changingPreset = true;
        PresetBox.SelectedItem = ModelPresets.All.FirstOrDefault(p =>
            p.BaseUrl.TrimEnd('/') == provider.BaseUrl.TrimEnd('/')) ?? ModelPresets.All[^1];
        _changingPreset = false;
        _discoveredModels = provider.Models;
        ModelSearchBox.Text = "";
        SetProtocol(provider.Protocol);
        SetContextWindow(provider.ContextWindowTokens);
        EditorTitle.Text = "编辑模型连接";
        PresetHint.Text = "修改配置后保存，即可在聊天中使用。切换服务商将新建一份配置。";
        NameBox.Text = provider.DisplayName;
        ProviderIdBox.Text = provider.ProviderId;
        ProviderIdBox.IsReadOnly = true;
        BaseUrlBox.Text = provider.BaseUrl;
        ModelsBox.Text = string.Join(Environment.NewLine, provider.Models);
        ApiKeyBox.Password = "";
        UpdateEndpointHints();
        AdvancedSettings.IsExpanded = false;
        EditorScroll.ChangeView(null, 0, null);
        SetStatus("已加载连接，可修改名称、密钥或模型列表。");
        RenderProviders();
        RenderModelOptions();
    }

    private void PresetBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (!_changingPreset && PresetBox.SelectedItem is ModelPreset preset) ApplyPreset(preset);
    }

    private void NewConnectionButton_Click(object sender, RoutedEventArgs e) => ApplyPreset(ModelPresets.All[0]);

    private void BaseUrlBox_TextChanged(object sender, TextChangedEventArgs e) => UpdateEndpointHints();

    private void UpdateEndpointHints()
    {
        if (KeyHint is null || ModelsHint is null || ApiKeyBox is null) return;
        bool local = Uri.TryCreate(BaseUrlBox.Text, UriKind.Absolute, out var uri) && ModelPresets.IsLocalEndpoint(uri);
        bool savedKey = _editing is { HasApiKey: true } &&
            _editing.BaseUrl.TrimEnd('/') == BaseUrlBox.Text.Trim().TrimEnd('/');
        ApiKeyBox.Header = local ? "API Key（可选）" : "API Key";
        ApiKeyBox.PlaceholderText = local ? "服务未启用认证时留空" : "粘贴服务商提供的密钥";
        KeyHint.Text = savedKey ? "已保存密钥；地址不变时留空可继续使用。" : local
            ? "直接连接指定服务；未启用认证时无需密钥。"
            : "密钥仅保存在本机，用于向所选服务发送请求。";
        ModelsHint.Text = local
            ? "获取服务已加载的模型，或填写服务提供的模型 ID。不需要安装 Ollama。"
            : "可编辑模型 ID，或获取账号支持的模型列表。";
    }

    private void NameBox_LostFocus(object sender, RoutedEventArgs e)
    {
        // Recognize a typed service name only in an untouched custom form.
        if (_editing is not null || PresetBox.SelectedItem is not ModelPreset { Id: "custom" } ||
            !string.IsNullOrWhiteSpace(BaseUrlBox.Text) || !string.IsNullOrWhiteSpace(ModelsBox.Text)) return;
        if (ModelPresets.Recognize(NameBox.Text) is not { } preset) return;
        string name = NameBox.Text;
        string key = ApiKeyBox.Password;
        ApplyPreset(preset);
        NameBox.Text = name;
        ApiKeyBox.Password = key;
    }

    private string[] ModelIds() => ModelsBox.Text.Split(['\r', '\n', ',', '，'],
        StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries).Distinct().ToArray();

    private void ModelsBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_updatingModelList) UpdateModelCount();
        else RenderModelOptions();
    }

    private void SetProtocol(string protocol) => ProtocolBox.SelectedItem = ProtocolBox.Items
        .OfType<ComboBoxItem>().FirstOrDefault(item => (string)item.Tag == protocol) ?? ProtocolBox.Items[0];

    private void SetContextWindow(int value)
    {
        string tokens = value.ToString(CultureInfo.InvariantCulture);
        CustomContextWindowBox.Text = tokens;
        ContextWindowBox.SelectedItem = ContextWindowBox.Items.OfType<ComboBoxItem>()
            .FirstOrDefault(item => (string)item.Tag == tokens)
            ?? ContextWindowBox.Items.OfType<ComboBoxItem>().First(item => (string)item.Tag == "custom");
        UpdateContextWindowVisibility();
    }

    private void ContextWindowBox_SelectionChanged(object sender, SelectionChangedEventArgs e) => UpdateContextWindowVisibility();

    private void UpdateContextWindowVisibility()
    {
        // SelectionChanged can fire while InitializeComponent is still creating the text box.
        if (CustomContextWindowBox is null || ContextWindowBox.SelectedItem is not ComboBoxItem option) return;
        bool custom = (string)option.Tag == "custom";
        CustomContextWindowBox.Visibility = custom ? Visibility.Visible : Visibility.Collapsed;
        if (!custom) CustomContextWindowBox.Text = (string)option.Tag;
    }

    private int ContextWindowTokens()
    {
        string value = ContextWindowBox.SelectedItem is ComboBoxItem { Tag: "custom" }
            ? CustomContextWindowBox.Text.Trim()
            : (ContextWindowBox.SelectedItem as ComboBoxItem)?.Tag?.ToString() ?? "";
        if (!int.TryParse(value, NumberStyles.None, CultureInfo.InvariantCulture, out int tokens))
            throw new InvalidOperationException("上下文窗口须为 2048–2000000 的整数（tokens）。");
        return ModelApiClient.ValidateContextWindowTokens(tokens);
    }

    private void ModelSearchBox_TextChanged(object sender, TextChangedEventArgs e) => RenderModelOptions();

    private string[] CandidateModelIds() => ((PresetBox.SelectedItem as ModelPreset)?.Models ?? [])
        .Concat(_discoveredModels).Concat(ModelIds()).Distinct(StringComparer.Ordinal).ToArray();

    private void UpdateModelCount()
    {
        if (ModelCount is not null) ModelCount.Text = $"共 {CandidateModelIds().Length} 个 · 已选 {ModelIds().Length} 个";
    }

    private void RenderModelOptions()
    {
        if (ModelOptions is null || ModelsBox is null || ModelSearchBox is null || EmptyModelsHint is null) return;
        ModelOptions.Children.Clear();
        var selected = ModelIds().ToHashSet();
        UpdateModelCount();
        var ids = CandidateModelIds();
        string search = ModelSearchBox.Text.Trim();
        foreach (string id in ids)
        {
            var detail = ModelCatalog.Describe(id);
            if (search.Length > 0 && !$"{detail.Name} {id}".Contains(search, StringComparison.OrdinalIgnoreCase)) continue;
            var content = new StackPanel { Spacing = 3 };
            content.Children.Add(new TextBlock { Text = detail.Name, FontSize = 13,
                FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, TextWrapping = TextWrapping.Wrap });
            if (detail.Name != id)
                content.Children.Add(new TextBlock { Text = id, FontSize = 11, TextWrapping = TextWrapping.Wrap,
                    Foreground = (Brush)Application.Current.Resources["KynxaSecondaryTextBrush"] });
            var option = new CheckBox { Content = content, IsChecked = selected.Contains(id),
                HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Stretch,
                Padding = new Thickness(10, 8, 10, 8), CornerRadius = new CornerRadius(8) };
            AutomationProperties.SetName(option, detail.Name == id ? id : $"{detail.Name}，{id}");
            option.Checked += (_, _) => ToggleModel(id, true);
            option.Unchecked += (_, _) => ToggleModel(id, false);
            ModelOptions.Children.Add(option);
        }
        EmptyModelsHint.Text = search.Length > 0 ? "没有匹配的模型。" : "暂时没有模型。可获取服务模型列表，或展开下方手动添加。";
        EmptyModelsHint.Visibility = ModelOptions.Children.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private void ToggleModel(string id, bool enabled)
    {
        _updatingModelList = true;
        var ids = ModelIds().Where(value => value != id).ToList();
        if (enabled) ids.Add(id);
        ModelsBox.Text = string.Join(Environment.NewLine, ids);
        UpdateModelCount();
        _updatingModelList = false;
    }

    private ModelConnection Form(bool requireModels)
    {
        if (!_loaded) throw new InvalidOperationException("尚未加载已保存连接，请关闭后重试，避免覆盖已有配置。");
        string id = ProviderIdBox.Text.Trim();
        string name = NameBox.Text.Trim();
        string url = BaseUrlBox.Text.Trim().TrimEnd('/');
        if (name.Length is < 1 or > 80) throw new InvalidOperationException("请填写连接名称（最多 80 个字符）。");
        if (!Regex.IsMatch(id, "^[a-z][a-z0-9-]{1,39}$"))
            throw new InvalidOperationException("请在高级设置中填写有效的连接 ID：2–40 位小写字母、数字或连字符。");
        if (_editing is null && _providers.Any(p => p.ProviderId == id))
            throw new InvalidOperationException("连接 ID 已存在，请修改高级设置中的 ID，或在左侧编辑已有连接。");
        if (!Uri.TryCreate(url, UriKind.Absolute, out var uri) || (uri.Scheme != "https" && uri.Scheme != "http"))
            throw new InvalidOperationException("请填写有效的服务地址。");
        if (uri.Scheme == "http" && !ModelPresets.IsLocalEndpoint(uri))
            throw new InvalidOperationException("公网服务请使用 HTTPS；本机和局域网服务可使用 HTTP。");
        if (string.IsNullOrWhiteSpace(ApiKeyBox.Password) && !ModelPresets.IsLocalEndpoint(uri) &&
            (_editing is not { HasApiKey: true } || _editing.BaseUrl.TrimEnd('/') != url))
            throw new InvalidOperationException("请填写此服务的 API Key。");
        var models = ModelIds();
        if (requireModels && models.Length == 0) throw new InvalidOperationException("请先获取模型，或手动填写模型 ID。");
        return new(id, name, url, models, string.IsNullOrWhiteSpace(ApiKeyBox.Password) ? null : ApiKeyBox.Password.Trim(),
            (string)((ComboBoxItem)ProtocolBox.SelectedItem).Tag, ContextWindowTokens());
    }

    private void SetBusy(bool busy)
    {
        EditorForm.IsHitTestVisible = ProviderPanel.IsHitTestVisible = !busy;
        PresetBox.IsEnabled = NameBox.IsEnabled = ApiKeyBox.IsEnabled = ModelsBox.IsEnabled =
            BaseUrlBox.IsEnabled = ProviderIdBox.IsEnabled = NewConnectionButton.IsEnabled = ProtocolBox.IsEnabled =
            ModelSearchBox.IsEnabled = ContextWindowBox.IsEnabled = CustomContextWindowBox.IsEnabled = !busy;
        foreach (var option in ModelOptions.Children.OfType<CheckBox>()) option.IsEnabled = !busy;
        foreach (var child in ProviderList.Children.OfType<Button>()) child.IsEnabled = !busy;
        ProbeButton.IsEnabled = SaveButton.IsEnabled = !busy;
        BusyIndicator.IsActive = busy;
    }

    private void SetStatus(string text, bool error = false)
    {
        if (_closed) return;
        StatusText.Text = text;
        ToolTipService.SetToolTip(StatusText, text);
        StatusText.Foreground = error ? new SolidColorBrush(Windows.UI.Color.FromArgb(255, 168, 57, 45))
            : (Brush)Application.Current.Resources["KynxaSecondaryTextBrush"];
    }

    private static string FriendlyError(Exception error) => error switch
    {
        HttpRequestException => "暂时无法连接 KYNXA 模型服务，请确认本机服务已启动后重试。",
        OperationCanceledException => "请求超时，请检查网络或服务地址后重试。",
        _ => error.Message
    };

    private async void TestConnectionButton_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            var input = Form(requireModels: false);
            SetBusy(true);
            SetStatus("正在连接服务并获取模型…");
            var result = await _api.TestAsync(input, _lifetime.Token);
            if (_closed) return;
            if (result.Models.Length > 0)
            {
                _discoveredModels = result.Models;
                var selected = ModelIds();
                var enabled = selected.Length == 0 && Uri.TryCreate(input.BaseUrl, UriKind.Absolute, out var endpoint) &&
                    ModelPresets.IsLocalEndpoint(endpoint) ? result.Models : selected.Intersect(result.Models);
                ModelsBox.Text = string.Join(Environment.NewLine, enabled);
                RenderModelOptions();
            }
            SetStatus(result.Models.Length > 0
                ? $"已获取 {result.Models.Length} 个模型 · {result.LatencyMs} ms。勾选需要的模型后保存。"
                : "服务已连接，未返回模型列表；已保留现有 ID，可手动填写后保存。");
        }
        catch (Exception error) { SetStatus($"获取失败：{FriendlyError(error)} 可保留预设 ID 直接保存。", true); }
        finally { if (!_closed) SetBusy(false); }
    }

    private async void SaveConnectionButton_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            var input = Form(requireModels: true);
            SetBusy(true);
            SetStatus("正在保存连接…");
            var provider = await _api.SaveAsync(input, _lifetime.Token);
            if (_closed) return;
            SelectProvider(provider);
            // Save has succeeded even if reloading the list subsequently fails.
            _providers = _providers.Where(p => p.ProviderId != provider.ProviderId).Append(provider).ToArray();
            RenderProviders();
            SetStatus($"已保存 {provider.DisplayName}。返回聊天页，选择模型即可使用。");
        }
        catch (Exception error) { SetStatus($"保存失败：{FriendlyError(error)}", true); }
        finally { if (!_closed) SetBusy(false); }
    }
}
