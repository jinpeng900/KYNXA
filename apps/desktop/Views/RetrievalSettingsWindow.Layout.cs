using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Views;

public sealed partial class RetrievalSettingsWindow
{
    private readonly StackPanel _settingsPanel = new() { Spacing = 10, Margin = new Thickness(24) };
    private readonly UserControl _settingsHost = new();
    private readonly StackPanel _sourcesPanel = new() { Spacing = 6 };
    private readonly InfoBar _notice = new() { IsClosable = true, IsOpen = false };
    private readonly TextBlock _embeddingStatus = new() { FontSize = 12, TextWrapping = TextWrapping.Wrap, Opacity = 0.65 };
    private readonly TextBlock _indexStatus = new() { FontSize = 12, TextWrapping = TextWrapping.Wrap, Opacity = 0.65 };
    private readonly CheckBox _inherit = new();
    private readonly ComboBox _localMode = new();
    private readonly ComboBox _semanticMode = new();
    private readonly ComboBox _rerankMode = new();
    private readonly ComboBox _webMode = new();
    private readonly ComboBox _webProvider = new();
    private readonly ComboBox _webDepth = new();
    private readonly ComboBox _webLanguage = new();
    private readonly Button _rebuild = new();
    private readonly Button _cancelJob = new();
    private readonly DispatcherTimer _jobTimer = new() { Interval = TimeSpan.FromSeconds(1) };

    private void BuildLayout()
    {
        var root = new Grid { RequestedTheme = ElementTheme.Light, Background = AppearanceService.GetBrush("KynxaMainBrush") };
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(44) });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        var focusBrush = AppearanceService.GetBrush("KynxaFocusBrush");
        foreach (string key in new[] { "ComboBoxBackgroundBorderBrushFocused", "SystemControlFocusVisualPrimaryBrush",
            "ComboBoxItemPillFillBrush", "CheckBoxCheckBackgroundFillChecked", "CheckBoxCheckBackgroundFillCheckedPointerOver",
            "CheckBoxCheckBackgroundFillCheckedPressed", "CheckBoxCheckBorderBrushChecked", "CheckBoxCheckBorderBrushCheckedPointerOver",
            "CheckBoxCheckBorderBrushCheckedPressed" })
            root.Resources[key] = focusBrush;
        foreach (string key in new[] { "ComboBoxItemBorderBrushSelected", "ComboBoxItemBorderBrushSelectedPointerOver", "ComboBoxItemBorderBrushSelectedPressed" })
            root.Resources[key] = AppearanceService.GetBrush("KynxaSelectionBrush");

        var titleBar = new Grid { Background = AppearanceService.GetBrush("KynxaTitleBarBrush") };
        var title = Label("KYNXA  /  检索与网页搜索");
        title.Margin = new Thickness(20, 0, 140, 0);
        title.VerticalAlignment = VerticalAlignment.Center;
        titleBar.Children.Add(title);
        root.Children.Add(titleBar);
        _settingsHost.Content = _settingsPanel;
        var scroll = new ScrollViewer { Content = _settingsHost, VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled };
        Grid.SetRow(scroll, 1);
        root.Children.Add(scroll);
        Content = root;
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(titleBar);
        _settingsPanel.Children.Add(_notice);
        AutomationProperties.SetAutomationId(_notice, "RetrievalSettingsNotice");

        if (_projectId is not null)
        {
            _settingsPanel.Children.Add(new TextBlock { Text = _projectName, FontSize = 14, TextWrapping = TextWrapping.Wrap });
            UiLocalization.Bind(_inherit, CheckBox.ContentProperty, "使用全局检索设置");
            AutomationProperties.SetAutomationId(_inherit, "RetrievalInheritGlobal");
            _settingsPanel.Children.Add(Row("设置范围", _inherit));
            _settingsPanel.Children.Add(Row("工作文件", Label("挂载后自动准备资料，可立即搜索和读取", 12, secondary: true)));
            if (!string.IsNullOrWhiteSpace(_mountedPath))
                _settingsPanel.Children.Add(new TextBlock { Text = _mountedPath, FontSize = 12, Opacity = 0.65,
                    TextWrapping = TextWrapping.Wrap });
        }
        ConfigurePicker(_localMode, "RetrievalLocalMode", "本地检索", ("true", "启用"), ("false", "停用"));
        ConfigurePicker(_semanticMode, "RetrievalSemanticMode", "语义检索", ("auto", "自动"), ("off", "停用"));
        ConfigurePicker(_rerankMode, "RetrievalRerankMode", "复杂查询重排", ("off", "停用"), ("builtin-multilingual-reranker", "按需启用"));
        _settingsPanel.Children.Add(Row("本地检索", _localMode));
        _settingsPanel.Children.Add(Row("语义检索", _semanticMode));
        _settingsPanel.Children.Add(Row("复杂查询重排", _rerankMode));
        _settingsPanel.Children.Add(Row("内置中英文模型", _embeddingStatus));

        var sourcesActions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        var addFile = ActionButton("添加文件", "RetrievalAddFile");
        var addFolder = ActionButton("添加文件夹", "RetrievalAddFolder");
        sourcesActions.Children.Add(addFile);
        sourcesActions.Children.Add(addFolder);
        _settingsPanel.Children.Add(Row(_projectId is null ? "全局资料" : "工作资料", sourcesActions));
        _settingsPanel.Children.Add(_sourcesPanel);
        var indexActions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        ConfigureButton(_rebuild, "继续准备资料", "RetrievalRebuildIndex");
        _rebuild.Visibility = Visibility.Collapsed;
        ConfigureButton(_cancelJob, "取消", "RetrievalCancelIndex");
        _cancelJob.Visibility = Visibility.Collapsed;
        indexActions.Children.Add(_rebuild);
        indexActions.Children.Add(_cancelJob);
        _settingsPanel.Children.Add(Row("资料准备", indexActions));
        _settingsPanel.Children.Add(_indexStatus);
        _settingsPanel.Children.Add(Label("网页搜索", 13, secondary: true));
        ConfigurePicker(_webMode, "RetrievalWebMode", "联网搜索", ("auto", "按需搜索"), ("off", "停用"));
        ConfigurePicker(_webProvider, "RetrievalWebProvider", "搜索服务", ("auto", "自动"));
        ConfigurePicker(_webDepth, "RetrievalWebDepth", "搜索深度", ("standard", "普通"), ("deep", "深入"));
        ConfigurePicker(_webLanguage, "RetrievalWebLanguage", "搜索语言", ("auto", "自动"), ("zh-CN", "简体中文"), ("en", "English"));
        _settingsPanel.Children.Add(Row("联网搜索", _webMode));
        _settingsPanel.Children.Add(Row("搜索服务", _webProvider));
        _settingsPanel.Children.Add(Row("搜索深度", _webDepth));
        _settingsPanel.Children.Add(Row("搜索语言", _webLanguage));
        var refresh = ActionButton("刷新", "RetrievalRefresh");
        _settingsPanel.Children.Add(refresh);
        refresh.HorizontalAlignment = HorizontalAlignment.Right;

        root.Loaded += async (_, _) =>
        {
            if (_loaded) return;
            _loaded = true;
            await RunOperationAsync(LoadAsync);
        };
        foreach (var picker in new[] { _localMode, _semanticMode, _rerankMode, _webMode, _webProvider, _webDepth, _webLanguage })
            picker.SelectionChanged += async (_, _) => { if (!_rendering) await SaveAsync(); };
        _inherit.Checked += async (_, _) => { if (!_rendering) await SaveAsync(); };
        _inherit.Unchecked += async (_, _) => { if (!_rendering) await SaveAsync(); };
        addFile.Click += async (_, _) => await ImportAsync(folder: false);
        addFolder.Click += async (_, _) => await ImportAsync(folder: true);
        _rebuild.Click += async (_, _) => await RunOperationAsync(async () => ShowJob(await _api.RebuildIndexAsync(_projectId, _lifetime.Token)));
        _cancelJob.Click += async (_, _) => await RunOperationAsync(async () =>
        {
            if (_job is { } job) ShowJob(await _api.CancelIndexJobAsync(job.JobId, _lifetime.Token));
        });
        refresh.Click += async (_, _) => await RunOperationAsync(LoadAsync);
        _jobTimer.Tick += async (_, _) => await PollJobAsync();
        _settingsHost.IsEnabled = false;
    }

    private static TextBlock Label(string key, double fontSize = 13, bool secondary = false)
    {
        var label = new TextBlock { FontFamily = (FontFamily)Application.Current.Resources["KynxaUIFont"], FontSize = fontSize,
            TextWrapping = TextWrapping.Wrap, Opacity = secondary ? 0.65 : 1, VerticalAlignment = VerticalAlignment.Center };
        UiLocalization.Bind(label, TextBlock.TextProperty, key);
        return label;
    }

    private static Grid Row(string key, FrameworkElement control)
    {
        var row = new Grid { ColumnSpacing = 16, Padding = new Thickness(14, 10, 14, 10), CornerRadius = new CornerRadius(10),
            Background = AppearanceService.GetBrush("KynxaSettingsCardBrush") };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.Children.Add(Label(key));
        control.HorizontalAlignment = HorizontalAlignment.Stretch;
        control.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(control, 1);
        row.Children.Add(control);
        return row;
    }

    private static void ConfigurePicker(ComboBox picker, string id, string label, params (string Value, string Label)[] options)
    {
        picker.FontFamily = (FontFamily)Application.Current.Resources["KynxaUIFont"];
        picker.FontSize = 13;
        picker.CornerRadius = new CornerRadius(6);
        picker.HorizontalAlignment = HorizontalAlignment.Stretch;
        AutomationProperties.SetAutomationId(picker, id);
        UiLocalization.Bind(picker, AutomationProperties.NameProperty, label);
        foreach (var (value, key) in options)
        {
            var item = new ComboBoxItem { Tag = value };
            UiLocalization.Bind(item, ComboBoxItem.ContentProperty, key);
            picker.Items.Add(item);
        }
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
        button.FontFamily = (FontFamily)Application.Current.Resources["KynxaUIFont"];
        button.FontSize = 13;
        button.Padding = new Thickness(10, 6, 10, 6);
        button.CornerRadius = new CornerRadius(6);
        UiLocalization.Bind(button, Button.ContentProperty, key);
        UiLocalization.Bind(button, AutomationProperties.NameProperty, key);
        AutomationProperties.SetAutomationId(button, id);
    }
}
