using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.System;
using Windows.UI.Core;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Confirms a single-line form without treating an IME candidate or repeated Enter as submission.
/// 为单行表单提供确认，避免将输入法选字或长按回车误判为提交。
/// </summary>
public sealed class SubmitTextBox : TextBox
{
    private static readonly (VirtualKey Key, VirtualKeyModifiers Modifier)[] ModifierKeys =
    [
        (VirtualKey.Shift, VirtualKeyModifiers.Shift), (VirtualKey.Control, VirtualKeyModifiers.Control),
        (VirtualKey.Menu, VirtualKeyModifiers.Menu), (VirtualKey.LeftWindows, VirtualKeyModifiers.Windows),
        (VirtualKey.RightWindows, VirtualKeyModifiers.Windows)
    ];
    private bool _composing;
    private bool _suppressEnter;

    public event EventHandler? SubmitRequested;

    public SubmitTextBox()
    {
        DefaultStyleKey = typeof(TextBox);
        PreviewKeyDown += InputPreviewKeyDown;
        KeyUp += InputKeyUp;
        TextCompositionStarted += CompositionStarted;
        TextCompositionEnded += CompositionEnded;
        LostFocus += (_, _) => _composing = _suppressEnter = false;
    }

    private void CompositionStarted(TextBox sender, TextCompositionStartedEventArgs args) => _composing = true;

    private void CompositionEnded(TextBox sender, TextCompositionEndedEventArgs args)
    {
        _composing = false;
        // Some IMEs end composition before forwarding the Enter that accepted their candidate.
        // 某些输入法会先结束组合输入，再转发用于确认候选文字的回车。
        _suppressEnter = true;
    }

    private void InputPreviewKeyDown(object sender, KeyRoutedEventArgs args)
    {
        if (args.Key != VirtualKey.Enter)
        {
            HandleShortcut(args.Key, VirtualKeyModifiers.None);
            return;
        }
        VirtualKeyModifiers modifiers = VirtualKeyModifiers.None;
        foreach (var (key, modifier) in ModifierKeys)
            if ((InputKeyboardSource.GetKeyStateForCurrentThread(key) & CoreVirtualKeyStates.Down) != 0) modifiers |= modifier;
        if (HandleShortcut(args.Key, modifiers, args.KeyStatus.WasKeyDown)) args.Handled = true;
    }

    private void InputKeyUp(object sender, KeyRoutedEventArgs args)
    {
        if (!_composing) _suppressEnter = false;
    }

    internal bool HandleShortcut(VirtualKey key, VirtualKeyModifiers modifiers, bool repeated = false)
    {
        if (!IsEnabled || !IsLoaded || AcceptsReturn || _composing) return false;
        if (key != VirtualKey.Enter)
        {
            _suppressEnter = false;
            return false;
        }
        if (_suppressEnter) return true;
        if (modifiers != VirtualKeyModifiers.None) return false;
        if (!repeated) SubmitRequested?.Invoke(this, EventArgs.Empty);
        return true;
    }
}
