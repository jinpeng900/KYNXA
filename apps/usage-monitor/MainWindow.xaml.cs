using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Threading;
using Kynxa.UsageMonitor.Core;

namespace Kynxa.UsageMonitor;

public partial class MainWindow : Window
{
    private readonly MonitorOptions _options;
    private readonly MonitorSettings _settings;
    private readonly CancellationTokenSource _lifetime = new();
    private readonly DispatcherTimer _ownerTimer = new() { Interval = TimeSpan.FromMilliseconds(500) };
    private readonly MenuItem _resetPositionMenu = new();
    private readonly MenuItem _topmostMenu = new() { IsCheckable = true };
    private readonly MenuItem _closeMenu = new();
    private Process? _ownerProcess;
    private Task? _inputTask;
    private UsageDisplayState _snapshot = new();
    private bool _closing;
    private bool _started;
    private bool _controlStarted;
    private bool _controlWarningShown;
    private bool _dragging;
    private Point _dragStart;
    private Rect _dragOriginalBounds;

    public MainWindow(MonitorOptions options)
    {
        _options = options;
        _settings = MonitorSettings.Load(options.DataDirectory);
        if (options.OwnerProcessId is int ownerId)
        {
            _ownerProcess = Process.GetProcessById(ownerId);
            // Hold the exact owner process handle before announcing readiness; PID reuse cannot adopt the card.
            // 就绪前立即持有准确的宿主进程句柄，避免进程号复用后卡片归属错误。
            _ = _ownerProcess.Handle;
            if (_ownerProcess.HasExited) throw new InvalidOperationException("owner_exited");
        }
        InitializeComponent();
        Topmost = _settings.Topmost;
        ShowInTaskbar = options.PreviewTaskbar;
        Title = options.Replay ? "KYNXA 用量统计 · 演示 / 回放" : "KYNXA 用量统计";
        ContextMenu = new ContextMenu();
        ContextMenu.SetResourceReference(Control.BackgroundProperty, "UsageSurfaceBrush");
        ContextMenu.SetResourceReference(Control.ForegroundProperty, "UsageTextBrush");
        ContextMenu.Items.Add(_resetPositionMenu);
        ContextMenu.Items.Add(_topmostMenu);
        ContextMenu.Items.Add(new Separator());
        ContextMenu.Items.Add(_closeMenu);
        _resetPositionMenu.Click += (_, _) => { WindowPlacement.MoveToBottomRight(this); SavePlacement(); };
        _topmostMenu.Click += (_, _) => { Topmost = _topmostMenu.IsChecked; _settings.Topmost = Topmost; SavePlacement(); };
        _closeMenu.Click += (_, _) => Close();
        _ownerTimer.Tick += OnOwnerTick;
        Loaded += OnLoaded;
        ContentRendered += OnContentRendered;
        Closing += OnClosing;
        RenderSnapshot(_snapshot, false);
        ApplyCollapsed();
    }

    private void OnLoaded(object sender, RoutedEventArgs e)
    {
        if (_started) return;
        _started = true;
        WindowPlacement.Restore(this, _settings.Left, _settings.Top);
        if (_ownerProcess is not null) _ownerTimer.Start();
        if (_options.Replay) _inputTask = Task.Run(ReadReplayAsync);
    }

    private void OnContentRendered(object? sender, EventArgs e)
    {
        if (!_options.ControlStdin || _controlStarted) return;
        _controlStarted = true;
        _inputTask = Task.Run(ReadOwnerControlAsync);
    }

    private async Task ReadOwnerControlAsync()
    {
        try
        {
            // Readiness proves the real card rendered, rather than merely proving Process.Start returned.
            // 就绪回执在真实卡片完成呈现后发送，不能把 Process.Start 返回当作界面启动成功。
            await Console.Out.WriteLineAsync(UsageControlReader.ReadyCommand);
            await Console.Out.FlushAsync();
            using Stream input = Console.OpenStandardInput();
            UsageControlReader reader = new();
            byte[] bytes = new byte[4_096];
            while (!_lifetime.IsCancellationRequested)
            {
                int read = await input.ReadAsync(bytes, _lifetime.Token);
                if (read == 0) break;
                bool closeRequested = false;
                reader.Append(bytes.AsSpan(0, read), snapshot =>
                {
                    if (!_closing && !Dispatcher.HasShutdownStarted)
                    {
                        // Synchronous dispatch bounds pending UI work even if the owner sends bursts of snapshots.
                        // 同步派发限制待处理界面任务，宿主集中发送快照时也不会形成无界队列。
                        Dispatcher.Invoke(() => { if (!_closing) RenderSnapshot(snapshot); });
                    }
                }, () => closeRequested = true, ReportInvalidCommand);
                if (closeRequested) break;
            }
        }
        catch (OperationCanceledException) when (_lifetime.IsCancellationRequested) { }
        catch (Exception error) when (error is IOException or ObjectDisposedException or InvalidOperationException or TaskCanceledException)
        {
            // Broken ownership pipes end the companion lifetime; no detached statistics process remains.
            // 宿主管道断开即结束悬浮卡生命周期，不遗留脱离宿主的统计进程。
        }
        finally
        {
            await CloseFromOwnerAsync();
        }
    }

    private void ReportInvalidCommand()
    {
        if (_controlWarningShown) return;
        _controlWarningShown = true;
        try { Console.Error.WriteLine("KYNXA_USAGE_MONITOR_INVALID_COMMAND_V1"); }
        catch (IOException) { }
    }

    private async Task ReadReplayAsync()
    {
        UsageAccumulator accumulator = new();
        UsageLogTail tail = new(_options.UsageLogPath!, true);
        try
        {
            while (!_lifetime.IsCancellationRequested)
            {
                await tail.ReadAvailableAsync(line => accumulator.AcceptLine(line), accumulator.Warn, _lifetime.Token);
                UsageSnapshot snapshot = accumulator.Snapshot;
                UsageDisplayState display = new()
                {
                    TotalText = snapshot.TokensText,
                    CacheText = snapshot.CacheText,
                    ScopeLabel = "演示 / 回放",
                    InputText = snapshot.KnownInputTokens.ToString("N0", CultureInfo.InvariantCulture),
                    OutputText = snapshot.KnownOutputTokens.ToString("N0", CultureInfo.InvariantCulture),
                    CacheHitText = snapshot.CacheHitTokens.ToString("N0", CultureInfo.InvariantCulture),
                    Description = snapshot.Describe(true),
                    IsIncomplete = snapshot.HasUnknownTotals || !snapshot.HasCompleteCache,
                };
                await Dispatcher.InvokeAsync(() => { if (!_closing) RenderSnapshot(display); });
                await Task.Delay(300, _lifetime.Token);
            }
        }
        catch (OperationCanceledException) when (_lifetime.IsCancellationRequested) { }
        catch (Exception error) when (error is IOException or InvalidOperationException or TaskCanceledException)
        {
            await CloseFromOwnerAsync();
        }
    }

    private void RenderSnapshot(UsageDisplayState snapshot, bool animate = true)
    {
        bool colorsChanged = _snapshot.Text != snapshot.Text || _snapshot.Accent != snapshot.Accent || !_started;
        _snapshot = snapshot;
        Resources["UsageSurfaceBrush"] = Brush(snapshot.Surface);
        Resources["UsageTextBrush"] = Brush(snapshot.Text);
        Resources["UsageSecondaryBrush"] = Brush(snapshot.Secondary);
        Resources["UsageBorderBrush"] = Brush(snapshot.Border);
        if (colorsChanged)
        {
            TokenNumber.SetForeground(Brush(snapshot.Text));
            CacheNumber.SetForeground(Brush(snapshot.Accent));
        }
        ScopeText.Text = snapshot.ScopeLabel;
        TokenLabel.Text = snapshot.TotalLabel;
        CacheLabel.Text = snapshot.CacheLabel;
        TokenNumber.SetText(snapshot.TotalText, animate);
        CacheNumber.SetText(snapshot.CacheText, animate);
        Card.ToolTip = snapshot.Description;
        _resetPositionMenu.Header = snapshot.ResetPositionLabel;
        _topmostMenu.Header = snapshot.TopmostLabel;
        _topmostMenu.IsChecked = Topmost;
        _closeMenu.Header = snapshot.CloseLabel;
        AutomationProperties.SetName(TokenNumber, snapshot.TotalLabel + " " + snapshot.TotalText);
        AutomationProperties.SetName(CacheNumber, snapshot.CacheLabel + " " + snapshot.CacheText);
        UpdateCollapseLabel();
    }

    private static SolidColorBrush Brush(string color)
    {
        SolidColorBrush brush = new((Color)ColorConverter.ConvertFromString(color));
        brush.Freeze();
        return brush;
    }

    private void OnCollapseClick(object sender, RoutedEventArgs e)
    {
        _settings.Collapsed = !_settings.Collapsed;
        ApplyCollapsed();
        WindowPlacement.MoveClamped(this, WindowPlacement.Bounds(this).Left, WindowPlacement.Bounds(this).Top,
            WindowPlacement.CursorPosition());
        SavePlacement();
    }

    private void ApplyCollapsed()
    {
        ValuesPanel.Visibility = _settings.Collapsed ? Visibility.Collapsed : Visibility.Visible;
        Height = _settings.Collapsed ? 64 : 150;
        CollapseButton.Content = _settings.Collapsed ? "⌃" : "⌄";
        UpdateCollapseLabel();
    }

    private void UpdateCollapseLabel()
    {
        string label = _settings.Collapsed ? _snapshot.ExpandLabel : _snapshot.CollapseLabel;
        CollapseButton.ToolTip = label;
        AutomationProperties.SetName(CollapseButton, label);
    }

    private void OnDragStart(object sender, MouseButtonEventArgs e)
    {
        if (e.ChangedButton != MouseButton.Left) return;
        _dragging = true;
        _dragStart = WindowPlacement.CursorPosition();
        _dragOriginalBounds = WindowPlacement.Bounds(this);
        DragHandle.CaptureMouse();
        e.Handled = true;
    }

    private void OnDragMove(object sender, MouseEventArgs e)
    {
        if (!_dragging) return;
        if (e.LeftButton != MouseButtonState.Pressed) { EndDrag(); return; }
        Point cursor = WindowPlacement.CursorPosition();
        WindowPlacement.MoveClamped(this, _dragOriginalBounds.Left + cursor.X - _dragStart.X,
            _dragOriginalBounds.Top + cursor.Y - _dragStart.Y, cursor);
    }

    private void OnDragEnd(object sender, MouseButtonEventArgs e) => EndDrag();
    private void OnDragCaptureLost(object sender, MouseEventArgs e) => EndDrag();

    private void EndDrag()
    {
        if (!_dragging) return;
        _dragging = false;
        DragHandle.ReleaseMouseCapture();
        SavePlacement();
    }

    private void SavePlacement()
    {
        if (!_started) return;
        Rect bounds = WindowPlacement.Bounds(this);
        _settings.Left = bounds.Left;
        _settings.Top = bounds.Top;
        try { _settings.Save(_options.DataDirectory); }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            // Placement persistence failure never alters the shared usage projection or ownership.
            // 位置保存失败不能改变共享用量投影或宿主生命周期。
        }
    }

    private void OnOwnerTick(object? sender, EventArgs e)
    {
        try { if (_ownerProcess?.HasExited == true) Close(); }
        catch (InvalidOperationException) { Close(); }
    }

    private async Task CloseFromOwnerAsync()
    {
        if (_closing || Dispatcher.HasShutdownStarted) return;
        try { await Dispatcher.InvokeAsync(() => { if (!_closing) Close(); }); }
        catch (TaskCanceledException) { }
    }

    private void OnClosing(object? sender, System.ComponentModel.CancelEventArgs e)
    {
        if (_closing) return;
        _closing = true;
        _lifetime.Cancel();
        _ownerTimer.Stop();
        EndDrag();
        SavePlacement();
        _ownerProcess?.Dispose();
        _ownerProcess = null;
    }
}
