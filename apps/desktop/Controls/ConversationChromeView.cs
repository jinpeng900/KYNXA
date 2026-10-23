using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using KYNXA_Desktop.Services;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Foundation;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Hosts owned Chrome as a native child inside the workspace viewport.
/// 将应用拥有的 Chrome 作为原生子窗口放入工作区视口。
/// </summary>
public sealed partial class ConversationChromeView : Grid, IDisposable
{
    private readonly ChromeWorkspaceApiClient _api = new();
    private readonly TaskCompletionSource _loaded = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private readonly CancellationTokenSource _lifetime = new();
    private readonly DispatcherQueueTimer _timer;
    private ChromeWorkspacePage? _page;
    private Task<ChromeWorkspacePage>? _opening;
    private Task? _closing;
    private Guid _conversationId;
    private long? _browserWindowId;
    private readonly Dictionary<long, nint> _browserWindows = [];
    private readonly Dictionary<nint, (double Left, double Top, double Right, double Bottom)> _windowInsets = [];
    private readonly List<(UIElement Element, long Token)> _visibilityCallbacks = [];
    private (double Left, double Top, double Right, double Bottom) _chromeUiInsets;
    private nint _owner, _host, _chrome, _geometryHook, _ownerHook;
    private WinEventCallback? _geometryChanged, _ownerChanged;
    private bool _active, _disposed, _reading, _positionQueued;
    private (int X, int Y, int Width, int Height) _hostBounds, _chromeBounds;
    public event EventHandler<ChromeWorkspaceState>? NavigationChanged;
    public string? Address { get; private set; }
    internal nint BrowserWindow => _chrome;
    internal nint HostWindow => _host;

    public ConversationChromeView()
    {
        InitializeBrowserInput();
        Loaded += (_, _) => _loaded.TrySetResult();
        LayoutUpdated += (_, _) => PositionWindow();
        Unloaded += (_, _) => SetActive(false);
        _timer = DispatcherQueue.CreateTimer();
        _timer.Interval = TimeSpan.FromSeconds(1);
        _timer.Tick += async (_, _) => await ReadStateAsync();
    }

    public async Task OpenAsync(Guid conversationId, Uri address)
    {
        if (_disposed) return;
        await _loaded.Task.WaitAsync(_lifetime.Token);
        WatchViewportVisibility();
        _conversationId = conversationId;
        // Observe late startup receipts so Rust can close the owned process.
        // 视图关闭后仍接收启动回执，以便关闭对应进程。
        _opening = _api.OpenAsync(conversationId, address, CancellationToken.None);
        _page = await _opening;
        if (_disposed) return;
        Address = address.AbsoluteUri;
        try
        {
            var deadline = Stopwatch.StartNew();
            while (_chrome == 0 && deadline.Elapsed < TimeSpan.FromSeconds(8))
            {
                _lifetime.Token.ThrowIfCancellationRequested();
                _chrome = FindOwnedWindow(_page.ProcessId, _page.Title, _page.WindowBounds);
                if (_chrome == 0) await Task.Delay(40, _lifetime.Token);
            }
            if (_chrome == 0) throw new InvalidOperationException(UiText.Get("Chrome 窗口尚未就绪，请刷新重试。"));
            _browserWindowId = _page.WindowId;
            if (_browserWindowId is long windowId) _browserWindows[windowId] = _chrome;
            _owner = Microsoft.UI.Win32Interop.GetWindowFromWindowId(XamlRoot.ContentIslandEnvironment.AppWindowId);
            // Reveal only the still-offscreen window so Chrome lays out its own toolbar before calibration.
            // 仅显示仍在屏幕外的窗口，让 Chrome 先完成自身工具栏布局，再一次性校准，不抢焦点。
            ShowWindow(_chrome, 4);
            await Task.Delay(40, _lifetime.Token);
            // The open receipt already identifies this window and viewport; show it before polling navigation state.
            // 打开回执已包含窗口和视口，先挂载显示，再按原有流程更新导航状态。
            InitializeContentInsets(_page.Viewport);
            AttachBrowserWindow();
            WatchContentGeometry(_page.ProcessId);
            WatchOwner();
            PositionWindow();
            SetActive(_active);
            EnumWindows((window, _) => { AttachBrowserBubble(window); return true; }, 0);
            await ReadStateAsync();
        }
        catch { Dispose(); throw; }
    }

    public async Task NavigateAsync(string action, Uri? address = null)
    {
        if (_page is null || _disposed) return;
        var state = await _api.ActAsync(_conversationId, _page.SessionId, action, address, _lifetime.Token);
        UpdateState(state);
    }

    private async Task ReadStateAsync()
    {
        if (_reading || !_active || _page is null || _disposed) return;
        PositionWindow();
        _reading = true;
        try { UpdateState(await _api.ActAsync(_conversationId, _page.SessionId, "state", token: _lifetime.Token)); }
        catch (OperationCanceledException) { }
        catch (Exception error) when (error is HttpRequestException or InvalidOperationException or IOException or System.Text.Json.JsonException)
        { Debug.WriteLine(error.Message); }
        finally { _reading = false; }
    }

    private void UpdateState(ChromeWorkspaceState state)
    {
        if (_disposed) return;
        if (state.WindowId is long windowId && _page is not null)
        {
            if (_browserWindowId is null) { _browserWindowId = windowId; _browserWindows[windowId] = _chrome; }
            else if (_browserWindowId != windowId)
            {
                nint window = _browserWindows.GetValueOrDefault(windowId);
                GetWindowThreadProcessId(window, out uint ownerProcess);
                if (window == 0 || !IsWindow(window) || ownerProcess != (uint)_page.ProcessId)
                    window = FindOwnedWindow(_page.ProcessId, state.Title, state.WindowBounds);
                if (window != 0)
                {
                    ReleaseBrowserInput();
                    if (_chrome != 0) ShowWindow(_chrome, 0);
                    _chrome = window; _browserWindowId = windowId;
                    _browserWindows[windowId] = window;
                    InitializeContentInsets(state.Viewport);
                    AttachBrowserWindow();
                    PositionWindow();
                }
            }
        }
        Address = state.Url ?? Address;
        NavigationChanged?.Invoke(this, state);
    }

    public void SetActive(bool active)
    {
        _active = active && !_disposed;
        // Disable input hit testing on inactive pages so underlying documents remain usable.
        // 非活动网页不接收输入，让下层文档正常滚动和翻页。
        IsHitTestVisible = _active;
        if (_active) { PositionWindow(); _timer.Start(); } else _timer.Stop();
        if (!_active) { ReleaseBrowserInput(); HideBrowserWindow(); }
    }

    private void PositionWindow()
    {
        if (_chrome == 0 || _host == 0 || _disposed) return;
        foreach (nint window in _browserWindows.Values)
            if (window != _chrome && IsWindowVisible(window)) ShowWindow(window, 0);
        var root = XamlRoot;
        if (!_active || root is null || !IsViewportVisible() || !IsWindowVisible(_owner) || IsIconic(_owner)
            || Microsoft.UI.Xaml.Media.VisualTreeHelper.GetOpenPopupsForXamlRoot(root).Count > 0)
        {
            HideBrowserWindow();
            return;
        }
        bool frameChanged = ApplyEmbeddedStyle();
        UpdateRendererInsets();
        var point = TransformToVisual(root.Content).TransformPoint(new Point());
        double scale = root.RasterizationScale;
        // Both child windows use parent-client physical pixels; the host itself clips everything outside the viewport.
        // 两级子窗口均使用父客户区物理像素，视口外的绘制由 Windows 父子窗口裁剪负责。
        var bounds = ((int)Math.Round(point.X * scale), (int)Math.Round(point.Y * scale),
            (int)Math.Round(ActualWidth * scale), (int)Math.Round(ActualHeight * scale));
        // Raise the viewport above the sibling ContentIsland surface while keeping the parent's clipping and focus.
        // 视口位于同级 ContentIsland 表面之上，仍由父窗口裁剪且不抢输入焦点。
        if (bounds != _hostBounds && SetWindowPos(_host, 0, bounds.Item1, bounds.Item2, bounds.Item3, bounds.Item4, 0x0010))
            _hostBounds = bounds;
        // ContentIsland can reorder its native surface on activation without changing layout.
        // Restore only the child viewport's sibling order; never raise the application above another app.
        // ContentIsland 激活时可能改原生层级但不改布局；只恢复视口子窗口层级，不把软件置顶。
        if (GetWindow(_host, 3) != 0) SetWindowPos(_host, 0, 0, 0, 0, 0, 0x0001 | 0x0002 | 0x0010);
        var insets = ContentInsets();
        var chromeBounds = (-insets.Left, -insets.Top,
            bounds.Item3 + insets.Left + insets.Right, bounds.Item4 + insets.Top + insets.Bottom);
        var actualOrigin = new NativePoint();
        bool hasActualBounds = GetWindowRect(_chrome, out var actual);
        if (hasActualBounds)
        {
            actualOrigin.X = actual.Left; actualOrigin.Y = actual.Top;
            hasActualBounds = ScreenToClient(_host, ref actualOrigin);
        }
        bool actualBoundsChanged = !hasActualBounds
            || actualOrigin.X != chromeBounds.Item1 || actualOrigin.Y != chromeBounds.Item2
            || actual.Right - actual.Left != chromeBounds.Item3 || actual.Bottom - actual.Top != chromeBounds.Item4;
        bool moved = chromeBounds != _chromeBounds || frameChanged || actualBoundsChanged;
        if (moved)
        {
            if (SetWindowPos(_chrome, 0, chromeBounds.Item1, chromeBounds.Item2, chromeBounds.Item3, chromeBounds.Item4,
                0x0004 | 0x0010 | 0x0020)) _chromeBounds = chromeBounds;
        }
        if (!IsWindowVisible(_chrome)) ShowWindow(_chrome, 4);
        if (!IsWindowVisible(_host)) ShowWindow(_host, 4);
    }

    private void AttachBrowserWindow()
    {
        if (_host == 0)
        {
            _host = CreateWindowEx(0, "STATIC", null, 0x40000000 | 0x02000000 | 0x04000000,
                0, 0, 0, 0, _owner, 0, 0, 0);
            if (_host == 0) throw new Win32Exception(Marshal.GetLastPInvokeError());
        }
        ShowWindow(_chrome, 0);
        ApplyEmbeddedStyle();
        if (GetParent(_chrome) != _host)
        {
            Marshal.SetLastPInvokeError(0);
            SetParent(_chrome, _host);
            if (GetParent(_chrome) != _host) throw new Win32Exception(Marshal.GetLastPInvokeError());
        }
        // A native parent replaces the former top-level owner and region workaround; never detach on a tab switch.
        // 原生父窗口取代旧的顶层 owner 与区域模拟方案；切换标签时只隐藏，不能解除父子关系。
        SetWindowRgn(_chrome, 0, true);
        SetWindowPos(_chrome, 0, 0, 0, 0, 0, 0x0001 | 0x0002 | 0x0004 | 0x0010 | 0x0020);
        _chromeBounds = default;
    }

    private void HideBrowserWindow()
    {
        if (_host != 0 && IsWindowVisible(_host)) ShowWindow(_host, 0);
        if (_chrome != 0 && IsWindowVisible(_chrome)) ShowWindow(_chrome, 0);
    }

    private bool IsViewportVisible()
    {
        if (!IsLoaded || XamlRoot is null || Math.Round(ActualWidth * XamlRoot.RasterizationScale) < 1
            || Math.Round(ActualHeight * XamlRoot.RasterizationScale) < 1) return false;
        for (DependencyObject? element = this; element is not null; element = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetParent(element))
            if (element is UIElement visual && visual.Visibility != Visibility.Visible) return false;
        return true;
    }

    private void WatchViewportVisibility()
    {
        foreach (var item in _visibilityCallbacks) item.Element.UnregisterPropertyChangedCallback(VisibilityProperty, item.Token);
        _visibilityCallbacks.Clear();
        // Visibility changes must hide native windows immediately, even when no composition frame is produced.
        // 可见性变化须立即隐藏原生窗口，即使锁屏或后台没有新的合成帧也不能等布局更新。
        for (DependencyObject? element = this; element is not null; element = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetParent(element))
            if (element is UIElement visual)
                _visibilityCallbacks.Add((visual, visual.RegisterPropertyChangedCallback(VisibilityProperty, (_, _) => PositionWindow())));
    }

    // Capture Chrome's client-drawn decoration before embedding. Hidden render-widget HWNDs may be tiny IME helpers.
    // 接入前记录 Chrome 客户区装饰；隐藏的 RenderWidget HWND 可能只是很小的输入法辅助窗口。
    private void InitializeContentInsets(ChromeWorkspaceViewport? viewport)
    {
        if (_windowInsets.TryGetValue(_chrome, out var cached)) { _chromeUiInsets = cached; return; }
        _chromeUiInsets = default;
        if (viewport is null || viewport.Width <= 0 || viewport.Height <= 0 || viewport.DeviceScaleFactor <= 0
            || !GetWindowRect(_chrome, out var browser)) return;
        var frame = NativeFrameInsets();
        double dpiScale = Math.Max(1, GetDpiForWindow(_chrome)) / 96d;
        double horizontal = Math.Max(0, browser.Right - browser.Left - viewport.Width * viewport.DeviceScaleFactor - frame.Left - frame.Right);
        double vertical = Math.Max(0, browser.Bottom - browser.Top - viewport.Height * viewport.DeviceScaleFactor - frame.Top - frame.Bottom);
        _chromeUiInsets = (0, vertical / dpiScale, horizontal / dpiScale, 0);
        _windowInsets[_chrome] = _chromeUiInsets;
    }

    // Narrow infobars may wrap; measure the visible page HWND, not CSS height.
    // 窄窗口的提示栏会换行；按可见页面 HWND 测量，避免 CSS 高度反复撑大窗口。
    private void UpdateRendererInsets()
    {
        if (!TryRendererWindow(out _, out var renderer) || !GetWindowRect(_chrome, out var browser)) return;
        var frame = NativeFrameInsets();
        double scale = Math.Max(1, GetDpiForWindow(_chrome)) / 96d;
        _chromeUiInsets = (Math.Max(0, renderer.Left - browser.Left - frame.Left) / scale,
            Math.Max(0, renderer.Top - browser.Top - frame.Top) / scale,
            Math.Max(0, browser.Right - renderer.Right - frame.Right) / scale,
            Math.Max(0, browser.Bottom - renderer.Bottom - frame.Bottom) / scale);
        _windowInsets[_chrome] = _chromeUiInsets;
    }

    private bool TryRendererWindow(out nint renderer, out WindowRectangle bounds)
    {
        nint selected = 0;
        WindowRectangle selectedBounds = default;
        long largestArea = 64 * 64;
        EnumChildWindows(_chrome, (window, _) =>
        {
            if (!IsWindowVisible(window) || !GetWindowRect(window, out var area)) return true;
            long pixels = (long)(area.Right - area.Left) * (area.Bottom - area.Top);
            if (pixels <= largestArea) return true;
            var name = new System.Text.StringBuilder(64);
            GetClassName(window, name, name.Capacity);
            if (name.ToString() != "Chrome_RenderWidgetHostHWND") return true;
            selected = window; selectedBounds = area; largestArea = pixels;
            return true;
        }, 0);
        renderer = selected; bounds = selectedBounds;
        return selected != 0;
    }

    private (int Left, int Top, int Right, int Bottom) ContentInsets()
    {
        var frame = NativeFrameInsets();
        double scale = Math.Max(1, GetDpiForWindow(_chrome)) / 96d;
        return (frame.Left + (int)Math.Round(_chromeUiInsets.Left * scale), frame.Top + (int)Math.Round(_chromeUiInsets.Top * scale),
            frame.Right + (int)Math.Round(_chromeUiInsets.Right * scale), frame.Bottom + (int)Math.Round(_chromeUiInsets.Bottom * scale));
    }

    // Only native nonclient borders may vary with the current frame; page layout never feeds back into window size.
    // 只随当前原生边框调整，网页排版尺寸不能反过来累积到窗口尺寸里。
    private (int Left, int Top, int Right, int Bottom) NativeFrameInsets()
    {
        var origin = new NativePoint();
        if (!GetWindowRect(_chrome, out var browser) || !GetClientRect(_chrome, out var client) || !ClientToScreen(_chrome, ref origin)) return default;
        return (Math.Max(0, origin.X - browser.Left), Math.Max(0, origin.Y - browser.Top),
            Math.Max(0, browser.Right - origin.X - client.Right), Math.Max(0, browser.Bottom - origin.Y - client.Bottom));
    }

    private bool ApplyEmbeddedStyle()
    {
        if (_chrome == 0) return false;
        long style = GetWindowLongPtr(_chrome, -16).ToInt64();
        long embedded = (style & ~(0x80000000L | 0x00C00000L | 0x00040000L | 0x00080000L | 0x00030000L))
            | 0x40000000L | 0x02000000L | 0x04000000L;
        long extended = GetWindowLongPtr(_chrome, -20).ToInt64();
        long embeddedExtended = (extended & ~(0x00000001L | 0x00000008L | 0x00000100L | 0x00000200L | 0x00020000L | 0x00040000L)) | 0x00000080L;
        if (style == embedded && extended == embeddedExtended) return false;
        SetWindowLongPtr(_chrome, -16, (nint)embedded);
        SetWindowLongPtr(_chrome, -20, (nint)embeddedExtended);
        SetWindowPos(_chrome, 0, 0, 0, 0, 0, 0x0001 | 0x0002 | 0x0004 | 0x0010 | 0x0020);
        return true;
    }

    // Native frame/layout changes do not necessarily trigger XAML layout; observe only this owned browser process.
    // 原生边框和布局变化不一定触发 XAML 布局；仅监听本次自有浏览器，离开时解除钩子。
    private void WatchContentGeometry(int processId)
    {
        _geometryChanged = (_, eventType, window, objectId, childId, _, _) =>
        {
            if (objectId != 0 || childId != 0 || _disposed) return;
            if (window == _chrome || _browserWindows.ContainsValue(window) || IsChild(_chrome, window)) QueuePosition();
            else if (eventType is 0x8002 or 0x800B)
                DispatcherQueue.TryEnqueue(() => AttachBrowserBubble(window));
        };
        _geometryHook = SetWinEventHook(0x8002, 0x800B, 0, _geometryChanged, (uint)processId, 0, 0);
    }

    // Embedded browser bubbles stay top-level; attach only its active frameless popups.
    // 嵌入后的气泡仍可能置顶；只接入当前浏览器的无框弹层，排除网页窗口和系统对话框。
    private void AttachBrowserBubble(nint window)
    {
        if (_disposed || !_active || _host == 0 || _page is null || !IsWindowVisible(_host) || !IsWindowVisible(window)) return;
        GetWindowThreadProcessId(window, out uint process);
        if (process != (uint)_page.ProcessId) return;
        long style = GetWindowLongPtr(window, -16).ToInt64();
        if ((style & 0x80000000L) == 0 || (style & 0x00C40000L) != 0) return;
        var name = new System.Text.StringBuilder(64);
        GetClassName(window, name, name.Capacity);
        if (!name.ToString().StartsWith("Chrome_WidgetWin_", StringComparison.Ordinal)) return;
        bool owned = false;
        for (nint owner = GetWindow(window, 4); owner != 0; owner = GetWindow(owner, 4))
            if (owner == _chrome || owner == _host || owner == _owner) { owned = true; break; }
        if (!owned || !GetWindowRect(window, out var bounds) || !GetClientRect(_host, out var viewport)) return;
        var origin = new NativePoint { X = bounds.Left, Y = bounds.Top };
        if (!ScreenToClient(_host, ref origin)) return;
        int width = bounds.Right - bounds.Left, height = bounds.Bottom - bounds.Top;
        ShowWindow(window, 0);
        SetWindowLongPtr(window, -16, (nint)((style & ~0x80000000L) | 0x40000000L | 0x04000000L));
        SetParent(window, _host);
        if (GetParent(window) != _host) return;
        SetWindowPos(window, 0, Math.Clamp(origin.X, 0, Math.Max(0, viewport.Right - width)),
            Math.Clamp(origin.Y, 0, Math.Max(0, viewport.Bottom - height)), width, height, 0x0010 | 0x0020);
        ShowWindow(window, 4);
    }

    // The native parent owns stacking and movement; observe shell visibility without stealing focus.
    // 原生父窗口负责层级和移动，仅监听软件可见状态且不抢焦点。
    private void WatchOwner()
    {
        GetWindowThreadProcessId(_owner, out uint ownerProcess);
        _ownerChanged = (_, eventType, window, objectId, childId, _, _) =>
        {
            if (objectId == 0 && childId == 0 && (window == _owner || eventType is 0x8002 or 0x8003)) QueuePosition();
        };
        _ownerHook = SetWinEventHook(0x8002, 0x800B, 0, _ownerChanged, ownerProcess, 0, 0);
    }

    private void QueuePosition()
    {
        if (_disposed || _positionQueued) return;
        _positionQueued = true;
        DispatcherQueue.TryEnqueue(() => { _positionQueued = false; PositionWindow(); });
    }

    // CDP IDs are not HWNDs; match title and bounds within the owned process.
    // CDP ID 不是 HWND；只在自有进程内按标题和边界匹配，不接入个人浏览器。
    private static nint FindOwnedWindow(int processId, string? title, ChromeWorkspaceWindowBounds? bounds)
    {
        var matches = new List<(nint Window, bool BoundsMatch)>();
        EnumWindows((window, _) =>
        {
            GetWindowThreadProcessId(window, out uint owner);
            if (owner != (uint)processId) return true;
            var name = new System.Text.StringBuilder(128);
            GetClassName(window, name, name.Capacity);
            if (name.ToString() != "Chrome_WidgetWin_1") return true;
            var caption = new System.Text.StringBuilder(1024);
            GetWindowText(window, caption, caption.Capacity);
            bool titleMatch = !string.IsNullOrWhiteSpace(title) && caption.ToString().StartsWith(title, StringComparison.Ordinal);
            bool boundsMatch = bounds is not null && GetWindowRect(window, out var rectangle)
                && Math.Abs(rectangle.Left - bounds.Left) <= 8 && Math.Abs(rectangle.Top - bounds.Top) <= 8
                && Math.Abs(rectangle.Right - rectangle.Left - bounds.Width) <= 8
                && Math.Abs(rectangle.Bottom - rectangle.Top - bounds.Height) <= 8;
            if (titleMatch || boundsMatch) matches.Add((window, boundsMatch));
            return true;
        }, 0);
        var exact = matches.Where(candidate => candidate.BoundsMatch).ToArray();
        return exact.Length == 1 ? exact[0].Window : matches.Count == 1 ? matches[0].Window : 0;
    }

    public void Dispose()
    {
        if (_disposed) return;
        SetActive(false); _disposed = true; _timer.Stop(); _lifetime.Cancel();
        foreach (var item in _visibilityCallbacks) item.Element.UnregisterPropertyChangedCallback(VisibilityProperty, item.Token);
        _visibilityCallbacks.Clear();
        if (_geometryHook != 0) { UnhookWinEvent(_geometryHook); _geometryHook = 0; }
        if (_ownerHook != 0) { UnhookWinEvent(_ownerHook); _ownerHook = 0; }
        foreach (nint window in _browserWindows.Values.Append(_chrome).Distinct().Where(window => window != 0))
        {
            GetWindowThreadProcessId(window, out uint process);
            if (_page is not null && process == (uint)_page.ProcessId)
                ShowWindow(window, 0);
        }
        _browserWindows.Clear(); _windowInsets.Clear(); _chrome = 0;
        _closing = CloseCoreAsync();
    }

    public Task CloseAsync()
    {
        Dispose();
        return _closing ?? Task.CompletedTask;
    }

    private async Task CloseCoreAsync()
    {
        try
        {
            var page = _page ?? (_opening is null ? null : await _opening);
            if (page is not null) await _api.ActAsync(_conversationId, page.SessionId, "close");
        }
        catch (Exception error) { Debug.WriteLine($"Chrome close: {error.GetType().Name}"); }
        finally
        {
            if (_host != 0) { DestroyWindow(_host); _host = 0; }
            _api.Dispose(); _lifetime.Dispose();
        }
    }

    private delegate bool EnumWindowsCallback(nint window, nint parameter);
    private delegate void WinEventCallback(nint hook, uint eventType, nint window, int objectId, int childId, uint threadId, uint time);
    [StructLayout(LayoutKind.Sequential)] private struct WindowRectangle { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] private struct NativePoint { public int X, Y; }
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumWindowsCallback callback, nint parameter);
    [DllImport("user32.dll")] private static extern bool EnumChildWindows(nint parent, EnumWindowsCallback callback, nint parameter);
    [DllImport("user32.dll")] private static extern bool GetWindowRect(nint window, out WindowRectangle rectangle);
    [DllImport("user32.dll")] private static extern bool GetClientRect(nint window, out WindowRectangle rectangle);
    [DllImport("user32.dll")] private static extern uint GetDpiForWindow(nint window);
    [DllImport("user32.dll")] private static extern bool IsWindow(nint window);
    [DllImport("user32.dll")] private static extern nint SetWinEventHook(uint eventMin, uint eventMax, nint module,
        WinEventCallback callback, uint processId, uint threadId, uint flags);
    [DllImport("user32.dll")] private static extern bool UnhookWinEvent(nint hook);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(nint window, out uint processId);
    [DllImport("user32.dll", EntryPoint = "GetWindowTextW", CharSet = CharSet.Unicode)] private static extern int GetWindowText(nint window, System.Text.StringBuilder title, int maximum);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetClassName(nint window, System.Text.StringBuilder name, int length);
    [DllImport("user32.dll")] private static extern bool ShowWindow(nint window, int command);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(nint window);
    [DllImport("user32.dll")] private static extern bool IsIconic(nint window);
    [DllImport("user32.dll")] private static extern bool ClientToScreen(nint window, ref NativePoint point);
    [DllImport("user32.dll")] private static extern bool ScreenToClient(nint window, ref NativePoint point);
    [DllImport("user32.dll", EntryPoint = "CreateWindowExW", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern nint CreateWindowEx(uint extendedStyle, string className, string? title, uint style,
        int x, int y, int width, int height, nint parent, nint menu, nint instance, nint parameter);
    [DllImport("user32.dll", SetLastError = true)] private static extern nint SetParent(nint window, nint parent);
    [DllImport("user32.dll")] private static extern nint GetParent(nint window);
    [DllImport("user32.dll")] private static extern nint GetWindow(nint window, uint kind);
    [DllImport("user32.dll")] private static extern bool DestroyWindow(nint window);
    [DllImport("user32.dll")] private static extern int SetWindowRgn(nint window, nint region, bool redraw);
    [DllImport("user32.dll")] private static extern bool SetWindowPos(nint window, nint after, int x, int y, int width, int height, uint flags);
    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")] private static extern nint GetWindowLongPtr(nint window, int index);
    [DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW", SetLastError = true)] private static extern nint SetWindowLongPtr(nint window, int index, nint value);
}
