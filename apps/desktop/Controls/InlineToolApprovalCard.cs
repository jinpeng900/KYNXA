using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Controls;

public sealed record ToolApprovalDecision(bool Approved, string Scope = "once");

/// <summary>
/// Shows a pending operation without interrupting the conversation or owning its approval request.
/// 在会话内展示待审批操作；控件不接管审批请求身份，也不打断当前输入。
/// </summary>
public sealed partial class InlineToolApprovalCard : UserControl
{
    private readonly Grid _actions = new() { ColumnSpacing = 8, RowSpacing = 8 };
    private readonly Grid _layout = new() { RowSpacing = 8 };
    private readonly Border _surface;
    private readonly TextBlock _title;
    private readonly ScrollViewer _contentScroll;
    private readonly Button _onceButton;
    private readonly Button _taskButton;
    private readonly Button _rejectButton;
    private readonly bool _taskApprovalAvailable;
    private bool? _actionsUseTwoRows;
    private bool _decisionSelected;
    private double _availableHeight = 580;

    public event Action<ToolApprovalDecision>? DecisionSelected;

    public InlineToolApprovalCard(ToolActivity tool)
    {
        ArgumentNullException.ThrowIfNull(tool);
        _taskApprovalAvailable = tool.TaskApprovalAvailable && !string.IsNullOrWhiteSpace(tool.TaskApprovalDescription);
        Name = "InlineToolApprovalCard";
        HorizontalAlignment = HorizontalAlignment.Stretch;
        HorizontalContentAlignment = HorizontalAlignment.Stretch;
        IsTabStop = false;
        AutomationProperties.SetAutomationId(this, Name);
        _surface = new Border
        {
            CornerRadius = new CornerRadius(14), Padding = new Thickness(12), BorderThickness = new Thickness(1),
            Background = AppearanceService.GetBrush("KynxaSettingsCardBrush"),
            BorderBrush = AppearanceService.GetBrush("KynxaDividerBrush")
        };

        var body = new StackPanel { Spacing = 8 };
        _title = Label("InlineToolApprovalTitle");
        _title.FontSize = 14;
        _title.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        UiLocalization.Bind(_title, TextBlock.TextProperty, "等待你的批准");
        AutomationProperties.SetLiveSetting(_title, Microsoft.UI.Xaml.Automation.Peers.AutomationLiveSetting.Polite);
        var summary = new TextBlock
        {
            Name = "InlineToolApprovalSummary", Text = string.IsNullOrWhiteSpace(tool.Summary) ? tool.Name : tool.Summary,
            FontFamily = (FontFamily)Application.Current.Resources["KynxaUIFont"], FontSize = 13,
            Foreground = AppearanceService.GetBrush("KynxaTextBrush"), TextWrapping = TextWrapping.Wrap,
            IsTextSelectionEnabled = true
        };
        const string sensitiveReadNotice = "读取可能含密钥的文件，批准后内容可能进入工具记录并发送给当前模型。";
        if (tool.Summary == sensitiveReadNotice) UiLocalization.Bind(summary, TextBlock.TextProperty, sensitiveReadNotice);
        body.Children.Add(summary);
        string? executionBoundaryKey = tool.Name.StartsWith("terminal.host.", StringComparison.Ordinal)
            ? "本机终端在沙箱外执行，可能修改电脑文件和设置。"
            : ComputerToolPresentation.Supports(tool.Name) ? "本机桌面操作在终端沙箱外执行。"
            : tool.Name.StartsWith("mcp.", StringComparison.Ordinal) ? "MCP 服务是受信任的外部程序，不属于终端沙箱。" : null;
        if (executionBoundaryKey is not null)
        {
            // Reuse complete execution boundaries in the readable details.
            // 详情完整保留原执行边界和授权范围，沿用审批规则。
            var executionBoundary = Label("InlineToolApprovalExecutionBoundary", secondary: true);
            UiLocalization.Bind(executionBoundary, TextBlock.TextProperty, executionBoundaryKey);
            body.Children.Add(executionBoundary);
        }

        var scope = new StackPanel { Spacing = 2 };
        var scopeLabel = Label("InlineToolApprovalScopeLabel", secondary: true);
        scopeLabel.FontSize = 12;
        UiLocalization.Bind(scopeLabel, TextBlock.TextProperty, "工作范围");
        scope.Children.Add(scopeLabel);
        var workspace = Label("InlineToolApprovalWorkspace");
        workspace.IsTextSelectionEnabled = true;
        if (tool.WorkspaceRoot is { Length: > 0 } workspaceRoot) workspace.Text = workspaceRoot;
        else UiLocalization.Bind(workspace, TextBlock.TextProperty, "范围未提供");
        scope.Children.Add(workspace);
        var scopeBoundary = Label("InlineToolApprovalBoundary", secondary: true);
        scopeBoundary.FontSize = 12;
        UiLocalization.Bind(scopeBoundary, TextBlock.TextProperty, tool.OutsideWorkspace is true
            ? "此操作超出工作目录。" : tool.OutsideWorkspace is false
                ? "此操作位于工作目录内。" : "此工具未提供本地文件范围。");
        scope.Children.Add(scopeBoundary);
        body.Children.Add(scope);

        _onceButton = ActionButton("InlineToolApprovalOnce", "批准一次", new(true));
        _taskButton = ActionButton("InlineToolApprovalTask", "本任务内授权", new(true, "task"));
        _taskButton.Visibility = _taskApprovalAvailable ? Visibility.Visible : Visibility.Collapsed;
        _rejectButton = ActionButton("InlineToolApprovalReject", "拒绝", new(false));
        _actions.Children.Add(_onceButton);
        _actions.Children.Add(_taskButton);
        _actions.Children.Add(_rejectButton);

        var taskScope = Label("InlineToolApprovalTaskScope", secondary: true);
        taskScope.FontSize = 12;
        taskScope.Visibility = _taskApprovalAvailable && !string.IsNullOrWhiteSpace(tool.TaskApprovalDescription)
            ? Visibility.Visible : Visibility.Collapsed;
        if (taskScope.Visibility == Visibility.Visible)
            UiLocalization.Bind(taskScope, TextBlock.TextProperty, tool.TaskApprovalDescription!);
        body.Children.Add(taskScope);

        var details = new Expander
        {
            Name = "InlineToolApprovalDetails", Content = ToolApprovalDialog.CreateOperationContent(tool), IsExpanded = false,
            HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Stretch
        };
        UiLocalization.Bind(details, Expander.HeaderProperty, "查看操作详情");
        details.Resources["ExpanderHeaderBackground"] = _surface.Background;
        details.Resources["ExpanderHeaderBorderBrush"] = _surface.BorderBrush;
        body.Children.Add(details);
        _contentScroll = new ScrollViewer
        {
            Name = "InlineToolApprovalContentScroll", Content = body, MaxHeight = 160,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled, HorizontalScrollMode = ScrollMode.Disabled,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto, VerticalScrollMode = ScrollMode.Enabled,
            HorizontalContentAlignment = HorizontalAlignment.Stretch
        };
        for (int row = 0; row < 3; row++) _layout.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        _layout.Children.Add(_title);
        Grid.SetRow(_contentScroll, 1);
        _layout.Children.Add(_contentScroll);
        Grid.SetRow(_actions, 2);
        _layout.Children.Add(_actions);
        _surface.Child = _layout;
        Content = _surface;

        // Scroll details between the fixed title and buttons; reserve composer space in short windows.
        // 详情在固定标题和按钮之间滚动；矮窗口仍为输入区留出空间。
        SizeChanged += (_, args) => { UpdateActionsLayout(args.NewSize.Width); UpdateContentHeight(); };
        _title.SizeChanged += (_, _) => UpdateContentHeight();
        _actions.SizeChanged += (_, _) => UpdateContentHeight();
        Loaded += (_, _) => { if (XamlRoot is { } root) SetAvailableHeight(root.Size.Height); };
        UpdateActionsLayout(0);
        UpdateContentHeight();
    }

    public void SetAvailableHeight(double availableHeight)
    {
        if (!double.IsFinite(availableHeight) || availableHeight <= 0) return;
        _availableHeight = availableHeight;
        UpdateContentHeight();
    }

    private void UpdateContentHeight()
    {
        double cardBudget = Math.Min(Math.Max(0, _availableHeight - 200), Math.Clamp(_availableHeight - 220, 140, 360));
        double titleHeight = Math.Max(20, Math.Max(_title.ActualHeight, _title.DesiredSize.Height));
        double actionsHeight = Math.Max(_actionsUseTwoRows is true ? 76 : 34,
            Math.Max(_actions.ActualHeight, _actions.DesiredSize.Height));
        double fixedHeight = titleHeight + actionsHeight + 2 * _layout.RowSpacing +
            _surface.Padding.Top + _surface.Padding.Bottom + _surface.BorderThickness.Top + _surface.BorderThickness.Bottom;
        // Drop the content minimum when fixed controls exhaust the available height.
        // 空间不足时取消正文最小高度，保留按钮高度，避免挤走输入框。
        double contentHeight = Math.Max(0, cardBudget - fixedHeight);
        if (Math.Abs(_contentScroll.MaxHeight - contentHeight) > 0.5) _contentScroll.MaxHeight = contentHeight;
    }

    private static TextBlock Label(string name, bool secondary = false) => new()
    {
        Name = name, FontFamily = (FontFamily)Application.Current.Resources["KynxaUIFont"], FontSize = 13,
        Foreground = AppearanceService.GetBrush(secondary ? "KynxaSecondaryTextBrush" : "KynxaTextBrush"),
        TextWrapping = TextWrapping.Wrap
    };

    private Button ActionButton(string name, string labelKey, ToolApprovalDecision decision)
    {
        var label = new TextBlock
        {
            Name = name + "Label", FontFamily = (FontFamily)Application.Current.Resources["KynxaUIFont"],
            FontSize = 13, TextWrapping = TextWrapping.Wrap, TextAlignment = TextAlignment.Center, MaxLines = 2
        };
        UiLocalization.Bind(label, TextBlock.TextProperty, labelKey);
        var button = new Button
        {
            Name = name, Content = label, MinWidth = 0, MinHeight = 34, Padding = new Thickness(8, 6, 8, 6),
            Style = (Style)Application.Current.Resources["KynxaQuietButtonStyle"], CornerRadius = new CornerRadius(8),
            HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Stretch,
            BorderThickness = new Thickness(1), BorderBrush = AppearanceService.GetBrush("KynxaDividerBrush")
        };
        AutomationProperties.SetAutomationId(button, name);
        UiLocalization.Bind(button, AutomationProperties.NameProperty, labelKey);
        button.Click += (_, _) => SelectDecision(decision);
        return button;
    }

    private void SelectDecision(ToolApprovalDecision decision)
    {
        // Latch before notifying the owner so double clicks and reentrant callbacks cannot authorize twice.
        // 通知调用方前先锁定本次决定，防止双击或事件重入重复提交授权。
        if (_decisionSelected) return;
        _decisionSelected = true;
        _onceButton.IsEnabled = _taskButton.IsEnabled = _rejectButton.IsEnabled = false;
        UiLocalization.Bind(_title, TextBlock.TextProperty, "正在确认审批…");
        DecisionSelected?.Invoke(decision);
    }

    private void UpdateActionsLayout(double width)
    {
        bool twoRows = _taskApprovalAvailable && width < 340;
        if (_actionsUseTwoRows == twoRows) return;
        _actionsUseTwoRows = twoRows;
        _actions.ColumnDefinitions.Clear();
        _actions.RowDefinitions.Clear();
        int columns = _taskApprovalAvailable && !twoRows ? 3 : 2;
        for (int column = 0; column < columns; column++)
            _actions.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        _actions.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        if (twoRows) _actions.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        Grid.SetColumn(_onceButton, 0);
        Grid.SetColumn(_taskButton, 1);
        Grid.SetColumn(_rejectButton, twoRows ? 0 : columns - 1);
        Grid.SetRow(_rejectButton, twoRows ? 1 : 0);
        Grid.SetColumnSpan(_rejectButton, twoRows ? 2 : 1);
    }
}
