using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;
using Windows.System;

namespace KYNXA_Desktop.Controls;

public sealed record WorkspaceFolderEdit(string Name, string? FolderPath);

/// <summary>A single folder draft, edited in the existing workspace pane.</summary>
public sealed class WorkspaceFolderEditor : StackPanel
{
    private readonly SubmitTextBox _name, _path;
    private readonly TextBlock _error;
    private readonly Grid _errorRow;
    private readonly Button _submit, _cancel, _withoutFolder;
    private readonly Func<WorkspaceFolderEdit, Task> _commit;
    private readonly bool _creating;
    private bool _busy, _closed;
    private UIElement? _pointerRoot;
    private readonly PointerEventHandler _outsidePointer;

    public event EventHandler<bool>? Closed;

    public WorkspaceFolderEditor(string title, string name, string? folderPath, bool creating,
        Func<WorkspaceFolderEdit, Task> commit, bool allowUnlink = false)
    {
        _creating = creating; _commit = commit;
        _outsidePointer = DismissErrorOnPointer;
        Spacing = 12; MaxWidth = 460; Margin = new Thickness(18);
        HorizontalAlignment = HorizontalAlignment.Stretch; VerticalAlignment = VerticalAlignment.Center;
        var neutral = Brush("KynxaSecondaryTextBrush");
        Children.Add(new TextBlock { Text = title, FontSize = 20, TextWrapping = TextWrapping.Wrap,
            Foreground = Brush("KynxaTextBrush") });
        _name = Input("ProjectCreationName", "项目名称", name);
        _name.MaxLength = 80;
        _name.Visibility = creating ? Visibility.Visible : Visibility.Collapsed;
        Children.Add(_name);
        Children.Add(new TextBlock { Text = UiText.Get(creating ? "工作文件夹（可选）" : "工作文件夹"), FontSize = 12, Foreground = neutral });
        _path = Input("WorkspaceFolderPath", "输入文件夹的完整路径", folderPath ?? string.Empty);
        _path.AllowDrop = true;
        _path.DragOver += (_, args) =>
        {
            args.AcceptedOperation = !_busy && args.DataView.Contains(StandardDataFormats.StorageItems)
                ? DataPackageOperation.Copy : DataPackageOperation.None;
            args.Handled = true;
        };
        _path.Drop += async (_, args) =>
        {
            args.Handled = true;
            if (_busy || !args.DataView.Contains(StandardDataFormats.StorageItems)) return;
            var deferral = args.GetDeferral();
            try
            {
                var items = await args.DataView.GetStorageItemsAsync();
                if (_closed) return;
                if (items.Count != 1 || items[0] is not StorageFolder folder)
                { ShowError(UiText.Get("一次只能关联一个文件夹。")); return; }
                _path.Text = folder.Path;
                if (_creating && string.IsNullOrWhiteSpace(_name.Text)) _name.Text = folder.DisplayName;
            }
            catch (Exception error) { if (!_closed) ShowError(error.Message); }
            finally { deferral.Complete(); }
        };
        Children.Add(_path);
        var errorBrush = new SolidColorBrush(Microsoft.UI.Colors.IndianRed);
        _error = new TextBlock { FontSize = 12, Foreground = errorBrush, TextWrapping = TextWrapping.NoWrap,
            TextTrimming = TextTrimming.CharacterEllipsis, MaxLines = 1, VerticalAlignment = VerticalAlignment.Center };
        _errorRow = new Grid { ColumnSpacing = 4, Visibility = Visibility.Collapsed };
        _errorRow.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        _errorRow.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        _errorRow.Children.Add(_error);
        var dismissError = new Button { Content = "×", FontSize = 13, Foreground = errorBrush,
            Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent), BorderThickness = new Thickness(0),
            MinWidth = 20, MinHeight = 20, Padding = new Thickness(2), CornerRadius = new CornerRadius(4) };
        AutomationProperties.SetName(dismissError, UiText.Get("关闭提示"));
        AutomationProperties.SetAutomationId(dismissError, "WorkspaceFolderErrorDismiss");
        dismissError.Click += (_, _) => HideError();
        Grid.SetColumn(dismissError, 1); _errorRow.Children.Add(dismissError);
        AutomationProperties.SetAutomationId(_error, "WorkspaceFolderError");
        AutomationProperties.SetLiveSetting(_error, Microsoft.UI.Xaml.Automation.Peers.AutomationLiveSetting.Polite);
        Children.Add(_errorRow);
        var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8,
            HorizontalAlignment = HorizontalAlignment.Right };
        _withoutFolder = Button(creating ? "不使用文件夹" : "取消关联文件夹", "WorkspaceFolderWithoutFolder");
        _withoutFolder.Visibility = creating || allowUnlink ? Visibility.Visible : Visibility.Collapsed;
        _withoutFolder.Click += async (_, _) => await SubmitAsync(withoutFolder: true);
        _cancel = Button("取消", "WorkspaceFolderCancel");
        _cancel.Click += (_, _) => Cancel();
        _submit = Button(creating ? "创建" : "保存", "WorkspaceFolderCommit");
        _submit.Click += async (_, _) => await SubmitAsync();
        actions.Children.Add(_withoutFolder); actions.Children.Add(_cancel); actions.Children.Add(_submit);
        Children.Add(actions);
        _path.TextChanged += (_, _) => HideError();
        _name.TextChanged += (_, _) => HideError();
        _path.SubmitRequested += async (_, _) => await SubmitAsync();
        _name.SubmitRequested += async (_, _) => await SubmitAsync();
        KeyDown += (_, args) =>
        {
            if (args.Key == VirtualKey.Escape) { args.Handled = true; Cancel(); }
        };
        Loaded += (_, _) =>
        {
            if (!_closed && _pointerRoot is null && XamlRoot?.Content is UIElement root)
            { _pointerRoot = root; root.AddHandler(UIElement.PointerPressedEvent, _outsidePointer, true); }
            FocusInput();
        };
        Unloaded += (_, _) => DetachPointerListener();
    }

    public void FocusInput() => (_creating ? _name : _path).Focus(FocusState.Programmatic);

    public void Cancel()
    {
        if (_closed) return;
        _closed = true; DetachPointerListener(); Closed?.Invoke(this, false);
    }

    private async Task SubmitAsync(bool withoutFolder = false)
    {
        if (_busy || _closed) return;
        HideError();
        _busy = true; SetEnabled(false);
        try
        {
            string? path = withoutFolder ? string.Empty : _path.Text.Trim().Trim('"');
            if (path.Length == 0)
            {
                if (!_creating && !withoutFolder) { ShowError(UiText.Get("请输入一个文件夹的完整路径。")); return; }
                path = null;
            }
            else
            {
                if (!Path.IsPathFullyQualified(path))
                { ShowError(UiText.Get("请输入文件夹的绝对路径。")); return; }
                path = Path.TrimEndingDirectorySeparator(Path.GetFullPath(path));
                if (!Directory.Exists(path))
                { ShowError(UiText.Get("文件夹不存在或无法访问，请检查路径。")); return; }
                // Validate the selected directory itself; no parent-folder authorization is required.
                await StorageFolder.GetFolderFromPathAsync(path);
                if (_closed) return;
            }
            string name = _name.Text.Trim();
            if (_creating && name.Length == 0 && path is not null) name = Path.GetFileName(path);
            if (name.Length == 0) name = UiText.Get("新建项目");
            await _commit(new(name, path));
            if (!_closed) { _closed = true; DetachPointerListener(); Closed?.Invoke(this, true); }
        }
        catch (OperationCanceledException) { Cancel(); }
        catch (Exception error) { if (!_closed) ShowError(error.Message); }
        finally { _busy = false; if (!_closed) SetEnabled(true); }
    }

    private void SetEnabled(bool enabled)
    {
        _name.IsEnabled = _path.IsEnabled = _submit.IsEnabled = _withoutFolder.IsEnabled = enabled;
        _cancel.IsEnabled = enabled;
    }
    public void ShowError(string message)
    {
        _error.Text = message.Replace('\r', ' ').Replace('\n', ' ');
        AutomationProperties.SetName(_error, _error.Text);
        _errorRow.Visibility = Visibility.Visible;
    }
    private void HideError() => _errorRow.Visibility = Visibility.Collapsed;
    private void DismissErrorOnPointer(object sender, PointerRoutedEventArgs args)
    {
        if (_errorRow.Visibility != Visibility.Visible) return;
        for (DependencyObject? element = args.OriginalSource as DependencyObject; element is not null;
            element = VisualTreeHelper.GetParent(element))
            if (ReferenceEquals(element, _errorRow)) return;
        HideError();
    }
    private void DetachPointerListener()
    {
        _pointerRoot?.RemoveHandler(UIElement.PointerPressedEvent, _outsidePointer);
        _pointerRoot = null;
    }
    private static SubmitTextBox Input(string id, string placeholder, string value)
    {
        var input = new SubmitTextBox { Text = value, PlaceholderText = UiText.Get(placeholder), FontSize = 13,
            MinHeight = 36, MinWidth = 0, CornerRadius = new CornerRadius(9), Padding = new Thickness(10, 6, 10, 6),
            Background = Brush("KynxaReplySurfaceBrush"), BorderBrush = Brush("KynxaDividerBrush"),
            Foreground = Brush("KynxaTextBrush"), FocusVisualPrimaryBrush = Brush("KynxaSecondaryTextBrush"),
            FocusVisualSecondaryBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent) };
        input.Resources["TextControlBorderBrushFocused"] = Brush("KynxaSecondaryTextBrush");
        input.Resources["TextControlBorderBrushPointerOver"] = Brush("KynxaSecondaryTextBrush");
        AutomationProperties.SetAutomationId(input, id); AutomationProperties.SetName(input, UiText.Get(placeholder));
        return input;
    }
    private static Button Button(string label, string id)
    {
        var button = new Button { Content = UiText.Get(label), CornerRadius = new CornerRadius(9), MinWidth = 0,
            Padding = new Thickness(10, 7, 10, 7), Background = Brush("KynxaReplySurfaceBrush"),
            BorderBrush = Brush("KynxaDividerBrush"), Foreground = Brush("KynxaTextBrush") };
        if (Application.Current.Resources.TryGetValue("KynxaQuietButtonStyle", out var style) && style is Style quiet) button.Style = quiet;
        AutomationProperties.SetAutomationId(button, id); AutomationProperties.SetName(button, UiText.Get(label));
        return button;
    }
    private static Brush Brush(string key) => (Brush)Application.Current.Resources[key];
}
