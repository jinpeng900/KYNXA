using System.ComponentModel;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Data;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Updates explicitly marked UI labels without inspecting user-authored content.
/// 只更新明确标记的 UI 文案，不检查用户自行编写的内容。
/// </summary>
public static class UiLocalization
{
    private static readonly object Sync = new();
    private static readonly List<WeakReference<LocalizedTextValue>> Sources = [];

    static UiLocalization() => UiText.LanguageChanged += (_, _) => Refresh();

    public static void Bind(DependencyObject target, DependencyProperty property, string key)
    {
        ArgumentNullException.ThrowIfNull(target);
        ArgumentNullException.ThrowIfNull(property);
        ArgumentNullException.ThrowIfNull(key);
        var source = new LocalizedTextValue(key, target.DispatcherQueue);
        // Native binding owns the value source for the target's lifetime, even after its managed wrapper is collected.
        // 原生绑定按目标生命周期持有值源；托管控件包装对象被回收后，界面仍能更新文字。
        BindingOperations.SetBinding(target, property, new Binding
        {
            Source = source, Path = new PropertyPath(nameof(LocalizedTextValue.Value)), Mode = BindingMode.OneWay
        });
        lock (Sync)
        {
            Sources.RemoveAll(reference => !reference.TryGetTarget(out _));
            Sources.Add(new(source));
        }
    }

    private static void Refresh()
    {
        LocalizedTextValue[] snapshot;
        lock (Sync)
        {
            Sources.RemoveAll(reference => !reference.TryGetTarget(out _));
            snapshot = Sources.Select(reference => reference.TryGetTarget(out var source) ? source : null)
                .OfType<LocalizedTextValue>().ToArray();
        }
        foreach (var source in snapshot) source.Refresh();
    }

    public static readonly DependencyProperty TextProperty = DependencyProperty.RegisterAttached(
        "Text", typeof(string), typeof(UiLocalization), new PropertyMetadata(null, (target, args) =>
        {
            DependencyProperty? property = target switch
            {
                TextBlock => TextBlock.TextProperty,
                MenuFlyoutItem => MenuFlyoutItem.TextProperty,
                MenuFlyoutSubItem => MenuFlyoutSubItem.TextProperty,
                _ => null
            };
            if (property is not null) Bind(target, property, (string?)args.NewValue ?? string.Empty);
        }));
    public static string GetText(DependencyObject target) => (string)target.GetValue(TextProperty);
    public static void SetText(DependencyObject target, string value) => target.SetValue(TextProperty, value);

    public static readonly DependencyProperty ContentProperty = DependencyProperty.RegisterAttached(
        "Content", typeof(string), typeof(UiLocalization), new PropertyMetadata(null, (target, args) =>
        {
            if (target is ContentControl) Bind(target, ContentControl.ContentProperty, (string?)args.NewValue ?? string.Empty);
        }));
    public static string GetContent(DependencyObject target) => (string)target.GetValue(ContentProperty);
    public static void SetContent(DependencyObject target, string value) => target.SetValue(ContentProperty, value);

    public static readonly DependencyProperty HeaderProperty = DependencyProperty.RegisterAttached(
        "Header", typeof(string), typeof(UiLocalization), new PropertyMetadata(null, (target, args) =>
        {
            DependencyProperty? property = target switch
            {
                TextBox => TextBox.HeaderProperty,
                PasswordBox => PasswordBox.HeaderProperty,
                ComboBox => ComboBox.HeaderProperty,
                Expander => Expander.HeaderProperty,
                ToggleSwitch => ToggleSwitch.HeaderProperty,
                AutoSuggestBox => AutoSuggestBox.HeaderProperty,
                _ => null
            };
            if (property is not null) Bind(target, property, (string?)args.NewValue ?? string.Empty);
        }));
    public static string GetHeader(DependencyObject target) => (string)target.GetValue(HeaderProperty);
    public static void SetHeader(DependencyObject target, string value) => target.SetValue(HeaderProperty, value);

    public static readonly DependencyProperty PlaceholderTextProperty = DependencyProperty.RegisterAttached(
        "PlaceholderText", typeof(string), typeof(UiLocalization), new PropertyMetadata(null, (target, args) =>
        {
            DependencyProperty? property = target switch
            {
                TextBox => TextBox.PlaceholderTextProperty,
                PasswordBox => PasswordBox.PlaceholderTextProperty,
                ComboBox => ComboBox.PlaceholderTextProperty,
                AutoSuggestBox => AutoSuggestBox.PlaceholderTextProperty,
                _ => null
            };
            if (property is not null) Bind(target, property, (string?)args.NewValue ?? string.Empty);
        }));
    public static string GetPlaceholderText(DependencyObject target) => (string)target.GetValue(PlaceholderTextProperty);
    public static void SetPlaceholderText(DependencyObject target, string value) => target.SetValue(PlaceholderTextProperty, value);

    public static readonly DependencyProperty ToolTipProperty = DependencyProperty.RegisterAttached(
        "ToolTip", typeof(string), typeof(UiLocalization), new PropertyMetadata(null,
            (target, args) => Bind(target, ToolTipService.ToolTipProperty, (string?)args.NewValue ?? string.Empty)));
    public static string GetToolTip(DependencyObject target) => (string)target.GetValue(ToolTipProperty);
    public static void SetToolTip(DependencyObject target, string value) => target.SetValue(ToolTipProperty, value);

    public static readonly DependencyProperty AutomationNameProperty = DependencyProperty.RegisterAttached(
        "AutomationName", typeof(string), typeof(UiLocalization), new PropertyMetadata(null,
            (target, args) => Bind(target, AutomationProperties.NameProperty, (string?)args.NewValue ?? string.Empty)));
    public static string GetAutomationName(DependencyObject target) => (string)target.GetValue(AutomationNameProperty);
    public static void SetAutomationName(DependencyObject target, string value) => target.SetValue(AutomationNameProperty, value);

    public static readonly DependencyProperty AutomationHelpProperty = DependencyProperty.RegisterAttached(
        "AutomationHelp", typeof(string), typeof(UiLocalization), new PropertyMetadata(null,
            (target, args) => Bind(target, AutomationProperties.HelpTextProperty, (string?)args.NewValue ?? string.Empty)));
    public static string GetAutomationHelp(DependencyObject target) => (string)target.GetValue(AutomationHelpProperty);
    public static void SetAutomationHelp(DependencyObject target, string value) => target.SetValue(AutomationHelpProperty, value);
}

/// <summary>
/// Native binding value with no reference to a control or window; the global registry holds only weak sources.
/// 原生绑定值不引用控件或窗口，全局注册仅弱持有值源，关闭窗口后可随绑定释放。
/// </summary>
[Microsoft.UI.Xaml.Data.Bindable]
public sealed class LocalizedTextValue : INotifyPropertyChanged
{
    private readonly string _key;
    private readonly DispatcherQueue _dispatcher;
    public event PropertyChangedEventHandler? PropertyChanged;
    public string Value => UiText.Get(_key);

    internal LocalizedTextValue(string key, DispatcherQueue dispatcher)
    { _key = key; _dispatcher = dispatcher; }

    internal void Refresh()
    {
        if (!_dispatcher.HasThreadAccess)
        {
            _dispatcher.TryEnqueue(Refresh);
            return;
        }
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Value)));
    }
}
