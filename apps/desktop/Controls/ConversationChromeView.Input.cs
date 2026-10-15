using System.Runtime.InteropServices;
using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.System;
using Windows.UI.Core;

namespace KYNXA_Desktop.Controls;

public sealed partial class ConversationChromeView
{
    private readonly TextBox _textInput = new()
    {
        Width = 1, Height = 1, Opacity = 0, IsTabStop = true, AcceptsReturn = true,
        HorizontalAlignment = HorizontalAlignment.Left, VerticalAlignment = VerticalAlignment.Top
    };
    private nint _pointerTarget, _keyboardTarget;
    private uint _pressedMouseButtons;
    private bool _composingText, _clearingText;

    private void InitializeBrowserInput()
    {
        // WinUI routes input through its ContentIsland even above a foreign child HWND.
        // Relay only this viewport's real input, as the WinUI WebView2 control does for its browser surface.
        // WinUI 经 ContentIsland 接收输入，即使画面上方是外部子 HWND；只转交本视口的真实输入。
        Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        Children.Add(_textInput);
        PointerPressed += BrowserPointerPressed;
        PointerReleased += BrowserPointerReleased;
        PointerMoved += BrowserPointerMoved;
        PointerWheelChanged += BrowserPointerWheelChanged;
        PointerCaptureLost += (_, _) => ReleaseBrowserPointer();
        PointerExited += (sender, args) =>
        {
            if (_pointerTarget == 0 && TryPointerTarget(args, out nint target, out _))
                PostMessage(target, 0x02A3, 0, 0);
        };
        AddHandler(PreviewKeyDownEvent, new KeyEventHandler(BrowserKeyDown), true);
        AddHandler(KeyUpEvent, new KeyEventHandler(BrowserKeyUp), true);
        // A native text box supplies committed IME text; CharacterReceived alone omits Chinese composition.
        // 原生文本框接收输入法提交文字；仅用 CharacterReceived 会漏掉中文组合输入。
        _textInput.TextCompositionStarted += (_, _) => _composingText = true;
        _textInput.TextCompositionEnded += (_, _) =>
        {
            _composingText = false;
            DispatcherQueue.TryEnqueue(CommitBrowserText);
        };
        _textInput.TextChanged += (_, _) => { if (!_composingText) CommitBrowserText(); };
        _textInput.LostFocus += (_, _) =>
        {
            if (XamlRoot is not null && !ReferenceEquals(FocusManager.GetFocusedElement(XamlRoot), _textInput)) ReleaseBrowserInput();
        };
    }

    private bool TryPointerTarget(PointerRoutedEventArgs args, out nint target, out NativePoint screen)
    {
        target = 0; screen = default;
        if (!_active || _disposed || _host == 0 || _chrome == 0 || !IsWindowVisible(_host)) return false;
        var point = args.GetCurrentPoint(this).Position;
        double scale = XamlRoot.RasterizationScale;
        screen = new NativePoint { X = (int)Math.Round(point.X * scale), Y = (int)Math.Round(point.Y * scale) };
        if (!ClientToScreen(_host, ref screen)) return false;
        target = _pointerTarget != 0 ? _pointerTarget : WindowFromPoint(screen);
        if (IsWindow(target) && IsChild(_host, target)) return true;
        // Lifted XAML hit testing can report its island surface; the pointer still belongs to this visible viewport.
        // XAML 输入路由可能命中岛表面；此时仍只向当前可见视口的真实页面窗口转交。
        return TryRendererWindow(out target, out _);
    }

    private static uint MouseKeys(PointerPointProperties properties)
    {
        uint keys = (properties.IsLeftButtonPressed ? 1u : 0) | (properties.IsRightButtonPressed ? 2u : 0)
            | (properties.IsMiddleButtonPressed ? 16u : 0) | (properties.IsXButton1Pressed ? 32u : 0)
            | (properties.IsXButton2Pressed ? 64u : 0);
        if (KeyPressed(VirtualKey.Shift)) keys |= 4;
        if (KeyPressed(VirtualKey.Control)) keys |= 8;
        return keys;
    }

    private static bool KeyPressed(VirtualKey key) =>
        (InputKeyboardSource.GetKeyStateForCurrentThread(key) & CoreVirtualKeyStates.Down) != 0;

    private static nint PointParameter(NativePoint point) => (nint)((point.X & 0xFFFF) | (point.Y << 16));

    private static void PostPointer(nint target, uint message, uint keys, NativePoint screen, bool screenCoordinates = false)
    {
        if (!screenCoordinates && !ScreenToClient(target, ref screen)) return;
        PostMessage(target, message, (nint)keys, PointParameter(screen));
    }

    private void BrowserPointerPressed(object sender, PointerRoutedEventArgs args)
    {
        if (!TryPointerTarget(args, out nint target, out var screen)) return;
        var properties = args.GetCurrentPoint(this).Properties;
        uint message = properties.PointerUpdateKind switch
        {
            PointerUpdateKind.RightButtonPressed => 0x0204,
            PointerUpdateKind.MiddleButtonPressed => 0x0207,
            PointerUpdateKind.XButton1Pressed or PointerUpdateKind.XButton2Pressed => 0x020B,
            _ => 0x0201
        };
        _keyboardTarget = _pointerTarget = target;
        var position = args.GetCurrentPoint(this).Position;
        _textInput.Margin = new Thickness(Math.Max(0, position.X), Math.Max(0, position.Y), 0, 0);
        _textInput.Focus(FocusState.Pointer);
        CapturePointer(args.Pointer);
        uint keys = MouseKeys(properties);
        _pressedMouseButtons = keys & (1u | 2u | 16u | 32u | 64u);
        if (message == 0x020B) keys |= (properties.IsXButton2Pressed ? 2u : 1u) << 16;
        PostPointer(target, message, keys, screen);
        args.Handled = true;
    }

    private void BrowserPointerReleased(object sender, PointerRoutedEventArgs args)
    {
        if (!TryPointerTarget(args, out nint target, out var screen)) return;
        var properties = args.GetCurrentPoint(this).Properties;
        uint message = properties.PointerUpdateKind switch
        {
            PointerUpdateKind.RightButtonReleased => 0x0205,
            PointerUpdateKind.MiddleButtonReleased => 0x0208,
            PointerUpdateKind.XButton1Released or PointerUpdateKind.XButton2Released => 0x020C,
            _ => 0x0202
        };
        uint keys = MouseKeys(properties);
        _pressedMouseButtons = keys & (1u | 2u | 16u | 32u | 64u);
        if (message == 0x020C) keys |= (properties.PointerUpdateKind == PointerUpdateKind.XButton2Released ? 2u : 1u) << 16;
        PostPointer(target, message, keys, screen);
        if (_pressedMouseButtons == 0) { _pointerTarget = 0; ReleasePointerCapture(args.Pointer); }
        args.Handled = true;
    }

    private void BrowserPointerMoved(object sender, PointerRoutedEventArgs args)
    {
        if (!TryPointerTarget(args, out nint target, out var screen)) return;
        PostPointer(target, 0x0200, MouseKeys(args.GetCurrentPoint(this).Properties), screen);
        args.Handled = true;
    }

    private void BrowserPointerWheelChanged(object sender, PointerRoutedEventArgs args)
    {
        if (!TryPointerTarget(args, out nint target, out var screen)) return;
        var properties = args.GetCurrentPoint(this).Properties;
        uint keys = MouseKeys(properties) | (unchecked((uint)properties.MouseWheelDelta) << 16);
        PostPointer(target, properties.IsHorizontalMouseWheel ? 0x020Eu : 0x020Au, keys, screen, screenCoordinates: true);
        args.Handled = true;
    }

    private void ReleaseBrowserPointer()
    {
        nint target = _pointerTarget;
        uint buttons = _pressedMouseButtons;
        _pointerTarget = 0; _pressedMouseButtons = 0;
        ReleasePointerCaptures();
        if (target == 0 || !IsWindow(target)) return;
        GetCursorPos(out var screen);
        if ((buttons & 1) != 0) PostPointer(target, 0x0202, 0, screen);
        if ((buttons & 2) != 0) PostPointer(target, 0x0205, 0, screen);
        if ((buttons & 16) != 0) PostPointer(target, 0x0208, 0, screen);
    }

    private void ReleaseBrowserInput()
    {
        ReleaseBrowserPointer();
        if (_keyboardTarget != 0 && IsWindow(_keyboardTarget)) PostMessage(_keyboardTarget, 0x0008, 0, 0);
        _keyboardTarget = 0;
    }

    private void BrowserKeyDown(object sender, KeyRoutedEventArgs args)
    {
        if (!_active || _keyboardTarget == 0 || _disposed || !ReferenceEquals(args.OriginalSource, _textInput)) return;
        if (_composingText || (uint)args.Key == 0xE5) return;
        SendBrowserKey(args, released: false);
        bool shortcut = KeyPressed(VirtualKey.Control) || KeyPressed(VirtualKey.Menu);
        if (shortcut || args.Key is VirtualKey.Back or VirtualKey.Tab or VirtualKey.Escape
            or VirtualKey.Delete or VirtualKey.Left or VirtualKey.Right or VirtualKey.Up or VirtualKey.Down
            or VirtualKey.Home or VirtualKey.End or VirtualKey.PageUp or VirtualKey.PageDown)
        {
            args.Handled = true;
            if (!shortcut && args.Key is VirtualKey.Back or VirtualKey.Tab or VirtualKey.Escape)
                PostMessage(_keyboardTarget, 0x0102, (nint)(uint)args.Key, 1);
        }
    }

    private void BrowserKeyUp(object sender, KeyRoutedEventArgs args)
    {
        if (!_active || _keyboardTarget == 0 || _disposed || !ReferenceEquals(args.OriginalSource, _textInput) || _composingText) return;
        SendBrowserKey(args, released: true);
        args.Handled = true;
    }

    private void SendBrowserKey(KeyRoutedEventArgs args, bool released)
    {
        var status = args.KeyStatus;
        uint parameter = Math.Max(1u, status.RepeatCount) | (status.ScanCode << 16)
            | (status.IsExtendedKey ? 1u << 24 : 0) | (released ? 0xC0000000 : status.WasKeyDown ? 1u << 30 : 0);
        bool system = args.Key == VirtualKey.Menu || status.IsMenuKeyDown;
        // Synchronous delivery bypasses TranslateMessage; committed text is sent exactly once by the IME sink.
        // 同步派发不经过对方队列的 TranslateMessage，文字仅由输入法接收框提交一次，避免重字。
        SendMessageTimeout(_keyboardTarget, released ? system ? 0x0105u : 0x0101u : system ? 0x0104u : 0x0100u,
            (nint)(uint)args.Key, (nint)parameter, 0x0002, 50, out _);
    }

    private void CommitBrowserText()
    {
        if (_clearingText || _composingText || _textInput.Text.Length == 0) return;
        string text = _textInput.Text;
        _clearingText = true;
        try { _textInput.Text = ""; }
        finally { _clearingText = false; }
        if (!_active || _keyboardTarget == 0 || _disposed) return;
        foreach (char character in text) PostMessage(_keyboardTarget, 0x0102, (nint)character, 1);
    }

    [DllImport("user32.dll")] private static extern nint WindowFromPoint(NativePoint point);
    [DllImport("user32.dll")] private static extern bool IsChild(nint parent, nint child);
    [DllImport("user32.dll")] private static extern bool GetCursorPos(out NativePoint point);
    [DllImport("user32.dll", EntryPoint = "PostMessageW")] private static extern bool PostMessage(nint window, uint message, nint wParam, nint lParam);
    [DllImport("user32.dll", EntryPoint = "SendMessageTimeoutW", SetLastError = true)] private static extern nint SendMessageTimeout(nint window, uint message,
        nint wParam, nint lParam, uint flags, uint timeoutMs, out nint result);
}
