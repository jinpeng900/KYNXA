using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Controls;

public sealed record UnknownOutcomeRequest(TaskRecord Task, string[] OperationIds, TaskModelReservationIdentity[]? ModelReservationIds = null,
    TaskOperationIdentity[]? Operations = null);

/// <summary>
/// Inline notice above the composer for a root task whose operations ended with unknown outcomes.
/// 输入区上方的行内提示：根任务有结果未知的操作；仅由用户点击确认或取消，不弹窗、不抢焦点。
/// </summary>
public sealed class ConversationUnknownOutcomeCard : UserControl
{
    private readonly Border _surface = new()
    {
        Padding = new Thickness(9, 6, 5, 6), CornerRadius = new CornerRadius(10), BorderThickness = new Thickness(1)
    };
    private readonly TextBlock _summary = new()
    {
        FontSize = 13, MaxLines = 2, TextWrapping = TextWrapping.Wrap, TextTrimming = TextTrimming.CharacterEllipsis,
        VerticalAlignment = VerticalAlignment.Center
    };
    private readonly TextBlock _feedback = new()
    {
        FontSize = 12, TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed
    };
    private string? _feedbackKey;
    private readonly TextBlock _queued = new()
    {
        FontSize = 12, TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed
    };
    private int _queuedCount;
    private readonly Button _confirm = ActionButton();
    private readonly Button _cancel = ActionButton();
    private UnknownOutcomeRequest? _request;
    private bool _busy, _languageSubscribed;

    public event EventHandler<UnknownOutcomeRequest>? ConfirmRequested;
    public event EventHandler<UnknownOutcomeRequest>? CancelRequested;

    public Guid? TaskId => _request?.Task.TaskId;

    public ConversationUnknownOutcomeCard()
    {
        Visibility = Visibility.Collapsed;
        FontFamily = Application.Current.Resources["KynxaUIFont"] as FontFamily;
        // Same neutral focus visuals as the queued-message rows; no default accent focus frame.
        // 与排队消息行一致的中性焦点样式，不出现默认的强调色焦点框。
        Resources["SystemControlFocusVisualPrimaryBrush"] = Brush("KynxaSecondaryTextBrush");
        Resources["SystemControlFocusVisualSecondaryBrush"] = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        var layout = new Grid { ColumnSpacing = 6 };
        layout.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        layout.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        var text = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        text.Children.Add(_summary);
        text.Children.Add(_queued);
        text.Children.Add(_feedback);
        layout.Children.Add(text);
        var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        actions.Children.Add(_confirm);
        actions.Children.Add(_cancel);
        Grid.SetColumn(actions, 1);
        layout.Children.Add(actions);
        _surface.Child = layout;
        _surface.Background = Brush("KynxaReplySurfaceBrush");
        _surface.BorderBrush = Brush("KynxaDividerBrush");
        _summary.Foreground = Brush("KynxaTextBrush");
        _feedback.Foreground = _queued.Foreground = Brush("KynxaSecondaryTextBrush");
        foreach (var button in new[] { _confirm, _cancel })
        {
            ApplyStyle(button, "KynxaQuietButtonStyle");
            button.FocusVisualPrimaryBrush = Brush("KynxaSecondaryTextBrush");
            button.FocusVisualSecondaryBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
            button.Foreground = Brush("KynxaSecondaryTextBrush");
        }
        _confirm.Click += (_, _) => { if (!_busy && _request is { } request) ConfirmRequested?.Invoke(this, request); };
        _cancel.Click += (_, _) => { if (!_busy && _request is { } request) CancelRequested?.Invoke(this, request); };
        Content = new Border { Child = _surface, Padding = new Thickness(0, 0, 0, 4) };
        Loaded += (_, _) =>
        {
            if (_languageSubscribed) return;
            _languageSubscribed = true;
            UiText.LanguageChanged += LanguageChanged;
            RefreshText();
        };
        Unloaded += (_, _) =>
        {
            if (!_languageSubscribed) return;
            _languageSubscribed = false;
            UiText.LanguageChanged -= LanguageChanged;
        };
    }

    public bool IsShown => Visibility == Visibility.Visible;

    /// <summary>Show or replace the notice without moving keyboard focus. / 显示或替换提示，不移动键盘焦点。</summary>
    public void Show(TaskRecord task, IReadOnlyList<TaskOperation> operations, IReadOnlyList<TaskModelReservation>? reservations = null)
    {
        if (_request?.Task.TaskId != task.TaskId) ShowFeedback(null);
        _request = new(task, operations.Select(operation => operation.Intent.OperationId).ToArray(),
            reservations?.Select(item => new TaskModelReservationIdentity(item.TaskId, item.ReservationId)).ToArray(),
            operations.Select(item => new TaskOperationIdentity(item.TaskId, item.Intent.OperationId)).ToArray());
        _toolNames = operations.Select(operation => operation.Intent.ToolName).ToArray();
        _modelReservationCount = reservations?.Count ?? 0;
        RefreshText();
        Visibility = Visibility.Visible;
    }

    /// <summary>Inline status text in the card; a UiText key or a gateway message. / 卡片内状态说明，不弹窗。</summary>
    public void ShowFeedback(string? keyOrMessage)
    {
        _feedbackKey = keyOrMessage;
        _feedback.Text = keyOrMessage is null ? "" : UiText.Get(keyOrMessage);
        _feedback.Visibility = keyOrMessage is null ? Visibility.Collapsed : Visibility.Visible;
    }

    public void Hide()
    {
        _request = null;
        _toolNames = [];
        _modelReservationCount = 0;
        ShowFeedback(null);
        SetQueuedCount(0);
        SetBusy(false);
        Visibility = Visibility.Collapsed;
    }

    /// <summary>The card covers the queue display, so it states how many queued messages are kept.
    /// 卡片覆盖排队区显示，因此说明仍保留的排队消息数量。</summary>
    public void SetQueuedCount(int count)
    {
        _queuedCount = count;
        _queued.Text = count > 0 ? string.Format(UiText.Get("{0} 条排队消息已保留，处理后恢复显示。"), count) : "";
        _queued.Visibility = count > 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    public void SetBusy(bool busy)
    {
        _busy = busy;
        _confirm.IsEnabled = _cancel.IsEnabled = !busy;
    }

    private string[] _toolNames = [];
    private int _modelReservationCount;

    private void LanguageChanged(object? sender, EventArgs args) => DispatcherQueue.TryEnqueue(RefreshText);

    private void RefreshText()
    {
        // Tool names are model-visible identifiers and stay untranslated; at most three are listed.
        // 工具名是原始标识，不翻译；最多列出三个，其余以计数体现。
        var labels = _toolNames.Concat(Enumerable.Repeat(UiText.Get("模型请求成本未知"), _modelReservationCount)).ToArray();
        string names = string.Join(" · ", labels.Take(3)) + (labels.Length > 3 ? " …" : "");
        _summary.Text = string.Format(UiText.Get("上次任务有 {0} 个操作结果未知：{1}"), labels.Length, names);
        string detail = UiText.Get("确认后不会重放这些操作，也不会视为成功；模型会先检查当前实际状态再决定下一步。");
        ToolTipService.SetToolTip(_summary, detail);
        AutomationProperties.SetName(this, UiText.Get("结果未知的操作"));
        AutomationProperties.SetHelpText(this, detail);
        _confirm.Content = UiText.Get("确认结果未知并继续");
        _cancel.Content = UiText.Get("取消任务");
        if (_feedbackKey is not null) _feedback.Text = UiText.Get(_feedbackKey);
        SetQueuedCount(_queuedCount);
        AutomationProperties.SetName(_confirm, UiText.Get("确认结果未知并继续"));
        AutomationProperties.SetHelpText(_confirm, detail);
        AutomationProperties.SetName(_cancel, UiText.Get("取消任务"));
    }

    private static Button ActionButton() => new()
    {
        MinHeight = 28, MinWidth = 0, Padding = new Thickness(7, 3, 7, 3), CornerRadius = new CornerRadius(8)
    };

    private static Brush? Brush(string key) => Application.Current.Resources.TryGetValue(key, out var resource) ? resource as Brush : null;
    private static void ApplyStyle(Button button, string key)
    {
        if (Application.Current.Resources.TryGetValue(key, out var resource) && resource is Style style) button.Style = style;
    }
}
