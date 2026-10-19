using System.Globalization;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Controls;

public enum ToolApprovalChoice { Once, Task, Reject }

/// <summary>
/// Presents one pending decision without modal windows or implicit authorization; the shell owns its identity and lifetime.
/// 非模态展示一条待处理审批，不隐式授权；主界面负责审批身份和生命周期。
/// </summary>
public sealed class ToolApprovalCard : UserControl
{
    private readonly Border _surface = new() { CornerRadius = new CornerRadius(16), BorderThickness = new Thickness(1), Padding = new Thickness(14, 10, 14, 10) };
    private readonly TextBlock _summary = new() { Name = "ToolApprovalSummary", FontSize = 14, TextWrapping = TextWrapping.Wrap, MaxLines = 2, TextTrimming = TextTrimming.CharacterEllipsis, VerticalAlignment = VerticalAlignment.Center };
    private readonly TextBlock _count = new() { FontSize = 12, VerticalAlignment = VerticalAlignment.Center };
    private readonly Button _detailsToggle = new() { Name = "ApprovalDetailsToggle", Width = 28, Height = 28, MinWidth = 0, MinHeight = 0, Padding = new Thickness(4), CornerRadius = new CornerRadius(8) };
    private readonly FontIcon _detailsIcon = new() { Glyph = "\uE70D", FontSize = 12 };
    private readonly TextBlock _details = new() { Name = "ToolApprovalDetails", FontSize = 12, TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true };
    private readonly ScrollViewer _detailsHost = new() { MaxHeight = 180, Visibility = Visibility.Collapsed, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled, HorizontalScrollMode = ScrollMode.Disabled, ZoomMode = ZoomMode.Disabled };
    private readonly TextBlock _error = new() { Name = "ToolApprovalError", FontSize = 12, MaxLines = 2, TextTrimming = TextTrimming.CharacterEllipsis, TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed };
    private readonly Grid _actions = new() { ColumnSpacing = 8, RowSpacing = 6, HorizontalAlignment = HorizontalAlignment.Right };
    private readonly Button _approveAll = DecisionButton("ApproveAllButton");
    private readonly Button _approveOnce = DecisionButton("ApproveOnceButton");
    private readonly Button _reject = DecisionButton("RejectApprovalButton");
    private ToolActivity? _tool;
    private int _index, _total;
    private bool _isSubmitting, _subscribed, _narrow;

    public event EventHandler<ToolApprovalChoice>? DecisionRequested;

    public ToolApprovalCard()
    {
        Name = "ToolApprovalCard";
        IsTabStop = false;
        ConfigureButtonResources("Button", "KynxaSettingsCardBrush", "KynxaControlHoverBrush", "KynxaControlPressedBrush", "KynxaTextBrush");
        ConfigureButtonResources("AccentButton", "KynxaSendBrush", "KynxaSendHoverBrush", "KynxaSendPressedBrush", "KynxaOnAccentBrush");
        Resources["SystemControlFocusVisualPrimaryBrush"] = AppearanceService.GetBrush("KynxaFocusBrush");
        FontFamily = Application.Current.Resources["KynxaUIFont"] as FontFamily;
        AutomationProperties.SetAutomationId(this, Name);
        AutomationProperties.SetAutomationId(_summary, _summary.Name);
        AutomationProperties.SetAutomationId(_detailsToggle, _detailsToggle.Name);
        AutomationProperties.SetLiveSetting(_summary, Microsoft.UI.Xaml.Automation.Peers.AutomationLiveSetting.Polite);
        var content = new StackPanel { Spacing = 8 };
        var heading = new Grid { ColumnSpacing = 8 };
        heading.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        heading.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        heading.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        heading.Children.Add(_summary);
        Grid.SetColumn(_count, 1);
        heading.Children.Add(_count);
        Grid.SetColumn(_detailsToggle, 2);
        heading.Children.Add(_detailsToggle);
        _detailsToggle.Content = _detailsIcon;
        _detailsHost.Content = _details;
        content.Children.Add(heading);
        content.Children.Add(_detailsHost);
        content.Children.Add(_error);
        content.Children.Add(_actions);
        _actions.Children.Add(_approveAll);
        _actions.Children.Add(_approveOnce);
        _actions.Children.Add(_reject);
        _surface.Child = content;
        Content = _surface;
        ApplyStyle(_detailsToggle, "KynxaCompactIconButtonStyle");
        ApplyStyle(_approveAll, "KynxaDialogButtonStyle");
        ApplyStyle(_approveOnce, "KynxaDialogAccentButtonStyle");
        ApplyStyle(_reject, "KynxaDialogButtonStyle");
        _detailsToggle.Click += (_, _) =>
        {
            _detailsHost.Visibility = _detailsHost.Visibility == Visibility.Visible ? Visibility.Collapsed : Visibility.Visible;
            RefreshDetailsToggle();
        };
        _approveAll.Click += (_, _) => RequestDecision(ToolApprovalChoice.Task);
        _approveOnce.Click += (_, _) => RequestDecision(ToolApprovalChoice.Once);
        _reject.Click += (_, _) => RequestDecision(ToolApprovalChoice.Reject);
        SizeChanged += (_, _) => UpdateActionLayout();
        RegisterPropertyChangedCallback(MaxHeightProperty, (_, _) => UpdateDetailsHeight());
        Loaded += (_, _) =>
        {
            if (!_subscribed)
            {
                _subscribed = true;
                UiText.LanguageChanged += LanguageChanged;
                AppearanceService.Changed += AppearanceChanged;
            }
            RefreshLanguage();
            RefreshAppearance();
            UpdateActionLayout();
        };
        Unloaded += (_, _) =>
        {
            if (!_subscribed) return;
            _subscribed = false;
            UiText.LanguageChanged -= LanguageChanged;
            AppearanceService.Changed -= AppearanceChanged;
        };
        SetActionColumns(false);
        RefreshLanguage();
        RefreshAppearance();
    }

    public void Configure(ToolActivity tool, int index, int total)
    {
        bool changed = _tool?.ApprovalId != tool.ApprovalId || _tool?.ToolCallId != tool.ToolCallId;
        _tool = tool;
        _index = Math.Max(1, index);
        _total = Math.Max(_index, total);
        if (changed)
        {
            _detailsHost.Visibility = Visibility.Collapsed;
            _error.Visibility = Visibility.Collapsed;
            SetSubmitting(false);
        }
        RefreshLanguage();
    }

    public void SetSubmitting(bool submitting)
    {
        _isSubmitting = submitting;
        _approveAll.IsEnabled = _approveOnce.IsEnabled = _reject.IsEnabled = !submitting;
        if (submitting) _error.Visibility = Visibility.Collapsed;
        RefreshSummary();
    }

    public void ShowError(string message)
    {
        SetSubmitting(false);
        _error.Text = message;
        _error.Visibility = string.IsNullOrWhiteSpace(message) ? Visibility.Collapsed : Visibility.Visible;
        ToolTipService.SetToolTip(_error, message);
        UpdateDetailsHeight();
    }

    private void RequestDecision(ToolApprovalChoice choice)
    {
        if (!_isSubmitting && _tool is not null) DecisionRequested?.Invoke(this, choice);
    }

    private void UpdateActionLayout()
    {
        bool narrow = ActualWidth > 0 && ActualWidth < 390;
        if (_narrow != narrow) SetActionColumns(narrow);
        _actions.Width = Math.Max(0, Math.Min(ActualWidth - 30, narrow ? 300 : 350));
        UpdateDetailsHeight();
    }

    private void UpdateDetailsHeight()
    {
        // Keep action rows visible before giving the optional details any vertical space.
        // 先给操作按钮留出空间，再把余下高度交给可选详情，避免短窗口裁掉审批操作。
        double reservedHeight = (_narrow ? 168 : 132) + (_error.Visibility == Visibility.Visible ? 42 : 0);
        _detailsHost.MaxHeight = double.IsFinite(MaxHeight) ? Math.Clamp(MaxHeight - reservedHeight, 0, 180) : 180;
    }

    private void SetActionColumns(bool narrow)
    {
        _narrow = narrow;
        _actions.ColumnDefinitions.Clear();
        _actions.RowDefinitions.Clear();
        for (int column = 0; column < (narrow ? 2 : 3); column++)
            _actions.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        _actions.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        if (narrow) _actions.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        Grid.SetRow(_approveAll, 0);
        Grid.SetColumn(_approveAll, 0);
        Grid.SetColumnSpan(_approveAll, narrow ? 2 : 1);
        Grid.SetRow(_approveOnce, narrow ? 1 : 0);
        Grid.SetColumn(_approveOnce, narrow ? 0 : 1);
        Grid.SetRow(_reject, narrow ? 1 : 0);
        Grid.SetColumn(_reject, narrow ? 1 : 2);
    }

    private void LanguageChanged(object? sender, EventArgs args) => DispatcherQueue.TryEnqueue(RefreshLanguage);
    private void AppearanceChanged(object? sender, EventArgs args) => DispatcherQueue.TryEnqueue(RefreshAppearance);

    private void RefreshLanguage()
    {
        SetButtonText(_approveAll, "全部批准");
        SetButtonText(_approveOnce, "批准一次");
        SetButtonText(_reject, "拒绝");
        ToolTipService.SetToolTip(_approveAll, UiText.Get("允许当前任务范围内的后续操作；范围外或敏感操作仍需单独确认，任务结束或取消后失效。"));
        _count.Text = _total > 1 ? $"{_index}/{_total}" : "";
        _count.Visibility = _total > 1 ? Visibility.Visible : Visibility.Collapsed;
        AutomationProperties.SetName(this, UiText.Get("工具操作审批"));
        RefreshSummary();
        RefreshDetailsToggle();
        if (_tool is not null) _details.Text = DetailsFor(_tool);
    }

    private void RefreshSummary()
    {
        string text = _isSubmitting ? UiText.Get("正在提交审批…") : _tool is null ? "" : SummaryFor(_tool);
        _summary.Text = text;
        ToolTipService.SetToolTip(_summary, text);
    }

    private void RefreshDetailsToggle()
    {
        bool expanded = _detailsHost.Visibility == Visibility.Visible;
        _detailsIcon.Glyph = expanded ? "\uE70E" : "\uE70D";
        string text = UiText.Get(expanded ? "收起审批详情" : "查看审批详情");
        AutomationProperties.SetName(_detailsToggle, text);
        ToolTipService.SetToolTip(_detailsToggle, text);
    }

    private void RefreshAppearance()
    {
        _surface.Background = AppearanceService.GetBrush("KynxaSettingsCardBrush");
        _surface.BorderBrush = AppearanceService.GetBrush("KynxaDividerBrush");
        _summary.Foreground = _details.Foreground = AppearanceService.GetBrush("KynxaTextBrush");
        _count.Foreground = _detailsToggle.Foreground = AppearanceService.GetBrush("KynxaSecondaryTextBrush");
        _error.Foreground = AppearanceService.GetBrush("KynxaErrorBrush");
    }

    private void ConfigureButtonResources(string prefix, string background, string hover, string pressed, string foreground)
    {
        // Resolve native button states against the same mutable palette brushes as the rest of the app.
        // 原生按钮各状态解析到应用原位更新的共享画刷，避免回退为系统默认蓝色或亮色闪帧。
        Resources[prefix + "Background"] = AppearanceService.GetBrush(background);
        Resources[prefix + "BackgroundPointerOver"] = AppearanceService.GetBrush(hover);
        Resources[prefix + "BackgroundPressed"] = AppearanceService.GetBrush(pressed);
        Resources[prefix + "BackgroundDisabled"] = AppearanceService.GetBrush("KynxaSendDisabledBrush");
        foreach (string suffix in new[] { "", "PointerOver", "Pressed" })
        {
            Resources[prefix + "Foreground" + suffix] = AppearanceService.GetBrush(foreground);
            Resources[prefix + "BorderBrush" + suffix] = AppearanceService.GetBrush("KynxaDividerBrush");
        }
        Resources[prefix + "ForegroundDisabled"] = AppearanceService.GetBrush("KynxaDisabledTextBrush");
        Resources[prefix + "BorderBrushDisabled"] = AppearanceService.GetBrush("KynxaDividerBrush");
    }

    internal static string SummaryFor(ToolActivity tool)
    {
        string Text(params string[] keys)
        {
            if (tool.Arguments is not { ValueKind: JsonValueKind.Object } args) return "";
            foreach (string key in keys)
                if (args.TryGetProperty(key, out var value) && value.ValueKind == JsonValueKind.String && !string.IsNullOrWhiteSpace(value.GetString()))
                    return value.GetString()!.Trim();
            return "";
        }
        string target = Text("url");
        if (tool.Name is "browser.open" or "browser.navigate" && Uri.TryCreate(target, UriKind.Absolute, out var uri) && uri.Host.Length > 0)
        {
            string host = uri.IdnHost + (uri.IsDefaultPort ? "" : $":{uri.Port}");
            return Format("允许打开 {0} 吗？", host.Length > 90 ? "…" + host[^88..] : host);
        }
        if (tool.Name.StartsWith("terminal.", StringComparison.Ordinal))
        {
            target = Text("script", "command");
            if (target.Length > 0) return Format(tool.Name.StartsWith("terminal.host.", StringComparison.Ordinal) ? "允许在本机运行命令 {0} 吗？" : "允许运行命令 {0} 吗？", CleanSummary(target));
        }
        target = CleanSummary(Text("path", "filePath", "sourcePath"));
        if (target.Length > 90) target = target[..28] + "…" + target[^59..];
        if (tool.Name is "filesystem.read" or "document.read" && tool.Summary == "读取可能含密钥的文件，批准后内容可能进入工具记录并发送给当前模型。")
            return Format("允许读取可能含密钥的文件 {0} 并将内容提供给当前模型吗？", target.Length > 0 ? target : tool.Name);
        if (tool.Name.StartsWith("filesystem.", StringComparison.Ordinal) || tool.Name == "document.read")
        {
            string key = tool.Name switch
            {
                "filesystem.read" or "filesystem.read_text" or "document.read" => "允许读取文件 {0} 吗？",
                "filesystem.write" or "filesystem.write_text" or "filesystem.edit" => "允许修改文件 {0} 吗？",
                "filesystem.delete" => "允许删除 {0} 吗？",
                "filesystem.move" => "允许移动 {0} 吗？",
                "filesystem.copy" => "允许复制 {0} 吗？",
                _ => "允许对 {0} 执行文件操作吗？"
            };
            if (target.Length > 0)
            {
                string summary = Format(key, target);
                return tool.OutsideWorkspace is true ? string.Format(CultureInfo.CurrentCulture, UiText.Get("工作目录外：{0}"), summary) : summary;
            }
        }
        if (ComputerToolPresentation.Supports(tool.Name)) return Format("允许在本机执行“{0}”吗？", UiText.Get(ComputerToolPresentation.ActionKey(tool.Name)));
        // Provider-written reasons are explanatory text, never the source of the action being authorized.
        // 模型填写的 reason 仅用于解释，不作为所授权操作的事实来源。
        return Format("允许执行 {0} 吗？", CleanSummary(tool.Name));
    }

    private static string DetailsFor(ToolActivity tool)
    {
        var lines = new List<string> { tool.Name };
        if (!string.IsNullOrWhiteSpace(tool.WorkspaceRoot)) lines.Add($"{UiText.Get("工作范围")}: {tool.WorkspaceRoot}");
        if (tool.OutsideWorkspace is true) lines.Add(UiText.Get("此操作超出工作目录。"));
        if (ComputerToolPresentation.Supports(tool.Name))
        {
            lines.Add(UiText.Get("本机桌面操作在终端沙箱外执行。"));
            foreach (var field in ComputerToolPresentation.Fields(tool))
                if (!string.IsNullOrWhiteSpace(field.Value)) lines.Add($"{UiText.Get(field.LabelKey)}: {(field.LocalizedValue ? UiText.Get(field.Value) : field.Value)}");
        }
        else if (tool.Arguments is { ValueKind: JsonValueKind.Object } args)
        {
            foreach (var property in args.EnumerateObject())
            {
                if (property.Name is "reason" or "policy" || property.Value.ValueKind is JsonValueKind.Null or JsonValueKind.Undefined) continue;
                string value = DisplayValue(property.Value);
                if (!string.IsNullOrWhiteSpace(value)) lines.Add($"{property.Name}: {value}");
            }
        }
        if (tool.Name.StartsWith("terminal.host.", StringComparison.Ordinal)) lines.Add(UiText.Get("本机终端在沙箱外执行，可能修改电脑文件和设置。"));
        if (tool.Name.StartsWith("mcp.", StringComparison.Ordinal)) lines.Add(UiText.Get("MCP 服务是受信任的外部程序，不属于终端沙箱。"));
        return string.Join("\n", lines);
    }

    private static string DisplayValue(JsonElement value) => value.ValueKind switch
    {
        JsonValueKind.String => value.GetString() ?? "",
        JsonValueKind.Array => string.Join(", ", value.EnumerateArray().Select(DisplayValue)),
        JsonValueKind.Object => string.Join("; ", value.EnumerateObject().Where(property => property.Name != "reason").Select(property => $"{property.Name}: {DisplayValue(property.Value)}")),
        _ => value.ToString()
    };

    private static string CleanSummary(string value)
    {
        string singleLine = string.Join(" ", value.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries));
        return singleLine.Length <= 90 ? singleLine : singleLine[..89] + "…";
    }

    private static string Format(string key, string value) => string.Format(CultureInfo.CurrentCulture, UiText.Get(key), CleanSummary(value));
    private static Button DecisionButton(string name)
    {
        var button = new Button { Name = name, MinWidth = 0, MinHeight = 34, Padding = new Thickness(10, 6, 10, 6), CornerRadius = new CornerRadius(8), HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Center };
        AutomationProperties.SetAutomationId(button, name);
        return button;
    }

    private static void SetButtonText(Button button, string key)
    {
        string text = UiText.Get(key);
        if (button.Content is not TextBlock label) button.Content = label = new TextBlock { TextWrapping = TextWrapping.Wrap, TextAlignment = TextAlignment.Center };
        label.Text = text;
        AutomationProperties.SetName(button, text);
    }

    private static void ApplyStyle(Button button, string key)
    {
        if (Application.Current.Resources.TryGetValue(key, out var value) && value is Style style) button.Style = style;
    }
}
