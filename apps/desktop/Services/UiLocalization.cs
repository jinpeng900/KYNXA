using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Updates explicitly marked UI labels without inspecting user-authored content.
/// 只更新明确标记的 UI 文案，不检查用户自行编写的内容。
/// </summary>
public static class UiLocalization
{
    private sealed class Registration(DependencyObject target, DependencyProperty property, string key)
    {
        public WeakReference<DependencyObject> Target { get; } = new(target);
        public DependencyProperty Property { get; } = property;
        public string Key { get; } = key;
        public volatile bool Active = true;
    }

    private static readonly object Sync = new();
    private static readonly List<Registration> Registrations = [];

    static UiLocalization() => UiText.LanguageChanged += (_, _) => Refresh();

    public static void Bind(DependencyObject target, DependencyProperty property, string key)
    {
        ArgumentNullException.ThrowIfNull(target);
        ArgumentNullException.ThrowIfNull(property);
        ArgumentNullException.ThrowIfNull(key);
        var registration = new Registration(target, property, key);
        lock (Sync)
        {
            Registrations.RemoveAll(existing =>
            {
                bool remove = !existing.Target.TryGetTarget(out var owner)
                    || (ReferenceEquals(owner, target) && existing.Property == property);
                if (remove) existing.Active = false;
                return remove;
            });
            Registrations.Add(registration);
        }
        Apply(registration);
    }

    private static void Refresh()
    {
        Registration[] snapshot;
        lock (Sync)
        {
            Registrations.RemoveAll(registration => !registration.Target.TryGetTarget(out _));
            snapshot = Registrations.ToArray();
        }
        foreach (var registration in snapshot) Apply(registration);
    }

    private static void Apply(Registration registration)
    {
        if (!registration.Active || !registration.Target.TryGetTarget(out var target)) return;
        if (target.DispatcherQueue.HasThreadAccess)
            target.SetValue(registration.Property, UiText.Get(registration.Key));
        else
            target.DispatcherQueue.TryEnqueue(() => Apply(registration));
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
