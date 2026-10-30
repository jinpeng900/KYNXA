using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.System;

namespace KYNXA_Desktop.Controls;

public sealed class QueuedPromptMoveEventArgs(QueuedPrompt prompt, int offset) : EventArgs
{
    public QueuedPrompt Prompt { get; } = prompt;
    public int Offset { get; } = offset;
}

/// <summary>
/// Displays pending messages above the composer; the shell owns dispatch and task continuation.
/// 在输入区上方显示等待消息；发送及任务接续仍由主界面流程负责。
/// </summary>
public sealed class ConversationPromptQueue : UserControl
{
    private readonly StackPanel _items = new() { Spacing = 4 };
    private readonly Dictionary<Guid, MessageRow> _rows = [];
    private QueuedPrompt[] _messages = [];
    private bool _canSteer, _languageSubscribed;

    public event EventHandler<QueuedPrompt>? SteerRequested;
    public event EventHandler<QueuedPrompt>? RemoveRequested;
    public event EventHandler<QueuedPromptMoveEventArgs>? MoveRequested;

    public ConversationPromptQueue()
    {
        Visibility = Visibility.Collapsed;
        FontFamily = Application.Current.Resources["KynxaUIFont"] as FontFamily;
        Resources["SystemControlFocusVisualPrimaryBrush"] = Brush("KynxaFocusBrush");
        Resources["SystemControlFocusVisualSecondaryBrush"] = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        Resources["TextControlBorderBrushFocused"] = Brush("KynxaFocusBrush");
        Content = new ScrollViewer
        {
            Content = _items, MaxHeight = 170,
            Style = (Style)Application.Current.Resources["KynxaFormScrollViewerStyle"],
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
            HorizontalScrollMode = ScrollMode.Disabled, ZoomMode = ZoomMode.Disabled
        };
        Loaded += (_, _) =>
        {
            if (_languageSubscribed) return;
            _languageSubscribed = true;
            UiText.LanguageChanged += LanguageChanged;
            RefreshLanguage();
        };
        Unloaded += (_, _) =>
        {
            if (!_languageSubscribed) return;
            _languageSubscribed = false;
            UiText.LanguageChanged -= LanguageChanged;
        };
    }

    public void ShowMessages(IReadOnlyList<QueuedPrompt> messages, bool canSteer)
    {
        _messages = messages.ToArray();
        _canSteer = canSteer;
        var visibleIds = _messages.Select(message => message.Id).ToHashSet();
        foreach (var removed in _rows.Keys.Where(id => !visibleIds.Contains(id)).ToArray())
        {
            _items.Children.Remove(_rows[removed].Surface);
            _rows.Remove(removed);
        }
        for (int index = 0; index < _messages.Length; index++)
        {
            var message = _messages[index];
            if (!_rows.TryGetValue(message.Id, out var row))
            {
                row = CreateRow(message);
                _rows.Add(message.Id, row);
            }
            row.Prompt = message;
            // Keep existing editors and selection while queue membership or execution state changes.
            // 队列及执行状态更新时复用编辑器和选区，不重建已存在的行。
            int currentIndex = _items.Children.IndexOf(row.Surface);
            if (currentIndex != index)
            {
                if (currentIndex >= 0) _items.Children.RemoveAt(currentIndex);
                _items.Children.Insert(index, row.Surface);
            }
            UpdateRow(row, index);
        }
        Visibility = _messages.Length == 0 ? Visibility.Collapsed : Visibility.Visible;
        AutomationProperties.SetName(this, UiText.Get("待发送消息"));
    }

    private MessageRow CreateRow(QueuedPrompt message)
    {
        var row = new MessageRow(message);
        row.Layout.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        row.Layout.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        row.Surface.Child = row.Layout;
        row.TextHost.Children.Add(row.Preview);
        row.TextHost.Children.Add(row.Editor);
        row.Layout.Children.Add(row.TextHost);
        var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        actions.Children.Add(row.Steer);
        actions.Children.Add(row.Delete);
        actions.Children.Add(row.More);
        Grid.SetColumn(actions, 1);
        row.Layout.Children.Add(actions);
        ApplyStyle(row.Steer, "KynxaQuietButtonStyle");
        ApplyStyle(row.Delete, "KynxaCompactIconButtonStyle");
        ApplyStyle(row.More, "KynxaCompactIconButtonStyle");
        foreach (var button in new[] { row.Steer, row.Delete, row.More })
        {
            button.FocusVisualPrimaryBrush = Brush("KynxaFocusBrush");
            button.FocusVisualSecondaryBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
            button.Foreground = Brush("KynxaSecondaryTextBrush");
        }
        row.Surface.Background = Brush("KynxaReplySurfaceBrush");
        row.Surface.BorderBrush = Brush("KynxaDividerBrush");
        row.Preview.Foreground = row.Editor.Foreground = Brush("KynxaTextBrush");
        row.Editor.FocusVisualPrimaryBrush = Brush("KynxaFocusBrush");
        row.Editor.FocusVisualSecondaryBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        row.Editor.BorderBrush = Brush("KynxaDividerBrush");
        row.More.Flyout = row.Menu;
        row.Menu.Items.Add(row.Edit);
        row.Menu.Items.Add(row.Up);
        row.Menu.Items.Add(row.Down);
        foreach (var item in new[] { row.Edit, row.Up, row.Down })
        {
            item.FocusVisualPrimaryBrush = Brush("KynxaFocusBrush");
            item.FocusVisualSecondaryBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        }
        row.Steer.Click += (_, _) =>
        {
            EndEdit(row);
            if (_canSteer && !row.Prompt.IsSubmitting) SteerRequested?.Invoke(this, row.Prompt);
        };
        row.Delete.Click += (_, _) =>
        {
            if (!row.Prompt.IsSubmitting) RemoveRequested?.Invoke(this, row.Prompt);
        };
        row.Edit.Click += (_, _) => DispatcherQueue.TryEnqueue(() => BeginEdit(row));
        row.Up.Click += (_, _) => Move(row, -1);
        row.Down.Click += (_, _) => Move(row, 1);
        row.Editor.TextChanged += (_, _) =>
        {
            if (row.IsEditing && !row.Prompt.IsSubmitting && !row.Prompt.IsTaskGuidance)
            {
                row.Prompt.Text = row.Editor.Text;
                row.Preview.Text = row.Prompt.DisplayText;
                ToolTipService.SetToolTip(row.Preview, row.Preview.Text);
            }
        };
        row.Editor.LostFocus += (_, _) => EndEdit(row);
        row.Editor.KeyDown += (_, args) =>
        {
            if (args.Key == VirtualKey.Escape)
            {
                row.Prompt.Text = row.OriginalText;
                EndEdit(row);
                args.Handled = true;
            }
        };
        UpdateLanguage(row);
        return row;
    }

    private void UpdateRow(MessageRow row, int index)
    {
        if (!row.IsEditing) { row.Preview.Text = row.Prompt.DisplayText; row.Editor.Text = row.Prompt.Text; }
        ToolTipService.SetToolTip(row.Preview, row.Preview.Text);
        bool interactive = !row.Prompt.IsSubmitting;
        bool editable = interactive && !row.Prompt.IsTaskGuidance;
        row.Editor.IsReadOnly = !editable;
        row.Steer.IsEnabled = interactive && _canSteer;
        row.Delete.IsEnabled = row.More.IsEnabled = interactive;
        row.Edit.IsEnabled = editable;
        row.Up.IsEnabled = interactive && index > 0 && !_messages[index - 1].IsSubmitting;
        row.Down.IsEnabled = interactive && index + 1 < _messages.Length && !_messages[index + 1].IsSubmitting;
    }

    private static void BeginEdit(MessageRow row)
    {
        if (row.Prompt.IsSubmitting || row.Prompt.IsTaskGuidance) return;
        row.OriginalText = row.Prompt.Text;
        row.Editor.Text = row.Prompt.Text;
        row.IsEditing = true;
        row.Preview.Visibility = Visibility.Collapsed;
        row.Editor.Visibility = Visibility.Visible;
        row.Editor.Focus(FocusState.Programmatic);
        row.Editor.SelectionStart = row.Editor.Text.Length;
    }

    private static void EndEdit(MessageRow row)
    {
        if (!row.IsEditing) return;
        row.IsEditing = false;
        row.Preview.Text = row.Prompt.DisplayText;
        ToolTipService.SetToolTip(row.Preview, row.Preview.Text);
        row.Preview.Visibility = Visibility.Visible;
        row.Editor.Visibility = Visibility.Collapsed;
    }

    private void Move(MessageRow row, int offset)
    {
        int index = Array.FindIndex(_messages, prompt => prompt.Id == row.Prompt.Id);
        int target = index + offset;
        if (row.Prompt.IsSubmitting || target < 0 || target >= _messages.Length || _messages[target].IsSubmitting) return;
        MoveRequested?.Invoke(this, new(row.Prompt, offset));
    }

    private void LanguageChanged(object? sender, EventArgs args) => DispatcherQueue.TryEnqueue(RefreshLanguage);
    private void RefreshLanguage()
    {
        AutomationProperties.SetName(this, UiText.Get("待发送消息"));
        foreach (var row in _rows.Values) UpdateLanguage(row);
    }

    private static void UpdateLanguage(MessageRow row)
    {
        row.Steer.Content = UiText.Get("引导");
        row.Edit.Text = UiText.Get("编辑排队消息");
        row.Up.Text = UiText.Get("上移");
        row.Down.Text = UiText.Get("下移");
        foreach (var (control, caption) in new (Control, string)[]
        {
            (row.Steer, "引导"), (row.Delete, "删除排队消息"), (row.More, "更多操作"), (row.Editor, "编辑排队消息")
        })
        {
            AutomationProperties.SetName(control, UiText.Get(caption));
            ToolTipService.SetToolTip(control, UiText.Get(caption));
        }
    }

    private static Brush? Brush(string key) => Application.Current.Resources.TryGetValue(key, out var resource) ? resource as Brush : null;
    private static void ApplyStyle(Button button, string key)
    {
        if (Application.Current.Resources.TryGetValue(key, out var resource) && resource is Style style) button.Style = style;
    }

    private sealed class MessageRow(QueuedPrompt prompt)
    {
        public QueuedPrompt Prompt { get; set; } = prompt;
        public bool IsEditing { get; set; }
        public string OriginalText { get; set; } = "";
        public Grid Layout { get; } = new() { ColumnSpacing = 6 };
        public Grid TextHost { get; } = new() { VerticalAlignment = VerticalAlignment.Center };
        public TextBlock Preview { get; } = new() { FontSize = 13, MaxLines = 2, TextWrapping = TextWrapping.Wrap, TextTrimming = TextTrimming.CharacterEllipsis };
        public TextBox Editor { get; } = new()
        {
            FontSize = 13, AcceptsReturn = true, TextWrapping = TextWrapping.Wrap, MinWidth = 0,
            MaxHeight = 96, Visibility = Visibility.Collapsed, CornerRadius = new CornerRadius(8), Padding = new Thickness(6, 4, 6, 4)
        };
        public Button Steer { get; } = new() { MinHeight = 28, MinWidth = 0, Padding = new Thickness(7, 3, 7, 3), CornerRadius = new CornerRadius(8) };
        public Button Delete { get; } = IconButton("\uE74D");
        public Button More { get; } = IconButton("\uE712");
        public MenuFlyout Menu { get; } = new();
        public MenuFlyoutItem Edit { get; } = new();
        public MenuFlyoutItem Up { get; } = new();
        public MenuFlyoutItem Down { get; } = new();
        public Border Surface { get; } = new()
        {
            Padding = new Thickness(9, 6, 5, 6), CornerRadius = new CornerRadius(10), BorderThickness = new Thickness(1)
        };

        private static Button IconButton(string glyph) => new()
        {
            Content = new FontIcon { Glyph = glyph, FontSize = 12 }, Width = 28, Height = 28,
            MinWidth = 0, MinHeight = 0, Padding = new Thickness(0), CornerRadius = new CornerRadius(8)
        };
    }
}
