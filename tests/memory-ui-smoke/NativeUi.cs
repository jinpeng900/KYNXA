using System.Runtime.InteropServices;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Automation.Provider;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace MemoryUiSmoke;

internal static class NativeUi
{
    public static void RequestClose(Window window)
    {
        // Window.Close() bypasses the native AppWindow.Closing request path.
        // Window.Close() 绕过原生 AppWindow.Closing 的关闭请求路径。
        if (!PostMessage(WinRT.Interop.WindowNative.GetWindowHandle(window), 0x0010, 0, 0))
            throw new InvalidOperationException("Posting a native WM_CLOSE request failed.");
    }

    [DllImport("user32.dll")] private static extern bool PostMessage(nint hwnd, uint message, nint wParam, nint lParam);

    public static IEnumerable<T> Descendants<T>(DependencyObject parent) where T : DependencyObject
    {
        if (parent is T owner) yield return owner;
        for (int index = 0; index < VisualTreeHelper.GetChildrenCount(parent); index++)
            foreach (var value in Descendants<T>(VisualTreeHelper.GetChild(parent, index))) yield return value;
    }

    public static T ById<T>(FrameworkElement root, string id) where T : FrameworkElement =>
        Descendants<T>(root).FirstOrDefault(element => AutomationProperties.GetAutomationId(element) == id)
        ?? throw new InvalidOperationException("Missing native control: " + id);

    public static T ByName<T>(FrameworkElement root, string name) where T : FrameworkElement =>
        Descendants<T>(root).FirstOrDefault(element => element.Name == name)
        ?? throw new InvalidOperationException("Missing native element: " + name);

    public static void Invoke(Button button)
    {
        if (!button.IsEnabled) throw new InvalidOperationException("Cannot invoke disabled native button: " + button.Name);
        var peer = FrameworkElementAutomationPeer.CreatePeerForElement(button);
        if (peer?.GetPattern(PatternInterface.Invoke) is not IInvokeProvider invoke)
            throw new InvalidOperationException("Native button does not expose Invoke automation: " + button.Name);
        invoke.Invoke();
    }

    public static void SetText(TextBox textBox, string value)
    {
        var peer = FrameworkElementAutomationPeer.CreatePeerForElement(textBox);
        if (peer?.GetPattern(PatternInterface.Value) is IValueProvider input) input.SetValue(value);
        else
        {
            // WinUI multiline editors expose Text rather than the writable Value pattern.
            // Setting the real control's Text still exercises its production TextChanged event.
            // WinUI 多行编辑器公开 Text 而非可写 Value 模式；设置真实控件的 Text 仍会触发生产 TextChanged 事件。
            textBox.Text = value;
        }
    }

    public static async Task InvokeListItemAsync(ListView list, object item)
    {
        list.ScrollIntoView(item);
        list.UpdateLayout();
        await Task.Delay(80);
        if (list.ContainerFromItem(item) is not FrameworkElement container)
            throw new InvalidOperationException("Native memory list item was not realized.");
        var peer = FrameworkElementAutomationPeer.CreatePeerForElement(container);
        if (peer?.GetPattern(PatternInterface.Invoke) is IInvokeProvider invoke) invoke.Invoke();
        else if (peer?.GetPattern(PatternInterface.SelectionItem) is ISelectionItemProvider selection) selection.Select();
        else throw new InvalidOperationException("Native list row exposes neither Invoke nor SelectionItem automation.");
        await Task.Delay(80);
    }

    public static ContentDialog? OpenDialog(FrameworkElement root)
    {
        foreach (var popup in VisualTreeHelper.GetOpenPopupsForXamlRoot(root.XamlRoot))
            if (popup.Child is not null && Descendants<ContentDialog>(popup.Child).FirstOrDefault() is { } dialog)
                return dialog;
        return Descendants<ContentDialog>(root).FirstOrDefault(dialog => dialog.Visibility == Visibility.Visible);
    }

    public static void InvokeDialogButton(ContentDialog dialog, bool primary)
    {
        string targetName = primary ? "PrimaryButton" : "CloseButton";
        string content = primary ? dialog.PrimaryButtonText : dialog.CloseButtonText;
        var button = Descendants<Button>(dialog).FirstOrDefault(candidate => candidate.Name == targetName)
            ?? Descendants<Button>(dialog).FirstOrDefault(candidate => candidate.Content?.ToString() == content);
        if (button is null) throw new InvalidOperationException("Native dialog has no " + targetName + " control.");
        Invoke(button);
    }

    public static bool IsVisible(FrameworkElement element)
    {
        DependencyObject? current = element;
        while (current is FrameworkElement frame)
        {
            if (frame.Visibility != Visibility.Visible) return false;
            current = VisualTreeHelper.GetParent(frame);
        }
        return element.ActualWidth > 0 && element.ActualHeight > 0;
    }
}
