using System.Diagnostics;
using System.Text;
using System.Text.Json;

namespace KYNXA_Desktop.Services;

public static partial class UsageMonitorService
{
    private static readonly SemaphoreSlim FloatingLock = new(1, 1);
    private static readonly SemaphoreSlim FloatingInputLock = new(1, 1);
    private static readonly JsonSerializerOptions SnapshotJson = new(JsonSerializerDefaults.Web);
    private static Process? _ownedCard;
    private static bool _cardReady;
    private static string? _floatingDataDirectory;
    private static int _sendingSnapshot;
    private static bool _floatingEnabled;
    private static bool _restoreFloating;
    public static bool FloatingEnabled => _floatingEnabled;
    public static string? LastFloatingError { get; private set; }
    private static string PreferencePath => Path.Combine(DataDirectory, "display.json");

    private static void LoadFloatingPreference()
    {
        try
        {
            // New card preference deliberately defaults off; the retired pet's enabled flag is not migrated.
            // 新卡片缺省关闭，不继承已放弃桌宠的开启状态。
            if (!File.Exists(PreferencePath)) return;
            using var document = JsonDocument.Parse(File.ReadAllText(PreferencePath));
            if (document.RootElement.ValueKind != JsonValueKind.Object) throw new JsonException();
            _restoreFloating = document.RootElement.TryGetProperty("floatingEnabled", out var enabled) && enabled.ValueKind == JsonValueKind.True;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or JsonException)
        { LastFloatingError = UiText.Get("悬浮卡设置未能读取，请重新开启。"); }
    }

    private static async Task RestoreFloatingAsync()
    {
        if (!_restoreFloating) return;
        _restoreFloating = false;
        await SetFloatingEnabledAsync(true);
    }

    private static async Task RefreshFloatingDirectoryAsync()
    {
        if (_floatingEnabled && _cardReady && IsProcessAlive(_ownedCard)
            && !string.Equals(_floatingDataDirectory, DataDirectory, StringComparison.OrdinalIgnoreCase))
        {
            // The child owns placement files in its startup directory; restart it after a storage migration.
            // 子卡持有启动时的位置目录；数据迁移后重启自有子卡，避免继续把设置写回旧目录。
            await SetFloatingEnabledAsync(true);
        }
    }

    public static async Task<string?> SetFloatingEnabledAsync(bool enabled)
    {
        if (StoragePaths.IsMigrating) return UiText.Get("数据正在迁移，请稍后更改用量显示。");
        _restoreFloating = false;
        await FloatingLock.WaitAsync();
        try
        {
            if (_closing) return null;
            if (enabled) await StartFloatingAsync();
            else await StopFloatingAsync();
            _floatingEnabled = enabled;
            Directory.CreateDirectory(DataDirectory);
            await File.WriteAllTextAsync(PreferencePath + ".tmp", JsonSerializer.Serialize(new { floatingEnabled = enabled }));
            File.Move(PreferencePath + ".tmp", PreferencePath, overwrite: true);
            LastFloatingError = null;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or InvalidOperationException
            or TimeoutException or OperationCanceledException or System.ComponentModel.Win32Exception)
        {
            _floatingEnabled = _cardReady && IsProcessAlive(_ownedCard);
            LastFloatingError = UiText.Get("悬浮统计卡未能启动或更新，请稍后重试。");
            Debug.WriteLine($"Usage card: {error.GetType().Name}");
        }
        finally { FloatingLock.Release(); NotifyChanged(); }
        return LastFloatingError;
    }

    private static async Task StartFloatingAsync()
    {
        if (IsProcessAlive(_ownedCard))
        {
            if (_cardReady && string.Equals(_floatingDataDirectory, DataDirectory, StringComparison.OrdinalIgnoreCase)) return;
            await StopFloatingAsync();
        }
        _ownedCard?.Dispose(); _ownedCard = null; _cardReady = false;
        if (_closing) throw new OperationCanceledException();
        string executable = Path.Combine(AppContext.BaseDirectory, "UsageMonitor", "KYNXA.UsageMonitor.exe");
        if (!File.Exists(executable)) throw new FileNotFoundException();
        Directory.CreateDirectory(DataDirectory);
        var start = new ProcessStartInfo(executable)
        {
            UseShellExecute = false, CreateNoWindow = true, RedirectStandardInput = true, RedirectStandardOutput = true,
            StandardInputEncoding = new UTF8Encoding(false), StandardOutputEncoding = new UTF8Encoding(false),
            WindowStyle = ProcessWindowStyle.Normal, WorkingDirectory = Path.GetDirectoryName(executable)!
        };
        start.ArgumentList.Add("--data-dir"); start.ArgumentList.Add(DataDirectory);
        start.ArgumentList.Add("--owner-pid"); start.ArgumentList.Add(Environment.ProcessId.ToString());
        start.ArgumentList.Add("--control-stdin");
        Process process = Process.Start(start) ?? throw new InvalidOperationException();
        _ownedCard = process;
        _floatingDataDirectory = DataDirectory;
        try
        {
            using var startup = CancellationTokenSource.CreateLinkedTokenSource(Lifetime.Token);
            startup.CancelAfter(TimeSpan.FromSeconds(10));
            string? signal = await process.StandardOutput.ReadLineAsync(startup.Token);
            if (signal != "KYNXA_USAGE_MONITOR_READY_V1" || process.HasExited || _closing) throw new InvalidOperationException();
            _cardReady = true;
            await WriteSnapshotAsync(process);
            _ = ObserveFloatingExitAsync(process);
        }
        catch
        {
            _cardReady = false;
            CloseProcessInput(process);
            try { await WaitForOwnedExitAsync(process, TimeSpan.FromSeconds(2)); }
            catch (Exception error) when (error is InvalidOperationException or System.ComponentModel.Win32Exception or TimeoutException)
            { Debug.WriteLine($"Usage card cleanup: {error.GetType().Name}"); }
            if (!IsProcessAlive(process))
            {
                if (ReferenceEquals(_ownedCard, process)) _ownedCard = null;
                process.Dispose();
            }
            throw;
        }
    }

    private static async Task StopFloatingAsync()
    {
        Process? process = _ownedCard;
        if (process is null) return;
        if (IsProcessAlive(process))
        {
            // EOF also reaches cards whose UI is temporarily busy and does not require a main-window handle.
            // EOF 不依赖主窗口句柄，界面暂忙时也能在恢复读取后结束卡片。
            CloseProcessInput(process);
            await WaitForOwnedExitAsync(process, TimeSpan.FromSeconds(5));
        }
        _ownedCard = null;
        _cardReady = false;
        _floatingDataDirectory = null;
        process.Dispose();
    }

    private static async Task ObserveFloatingExitAsync(Process process)
    {
        try
        {
            await process.WaitForExitAsync();
            await FloatingLock.WaitAsync();
            try
            {
                if (!ReferenceEquals(_ownedCard, process)) return;
                _ownedCard = null;
                _cardReady = false;
                _floatingDataDirectory = null;
                _floatingEnabled = false;
                if (!_closing)
                {
                    Directory.CreateDirectory(DataDirectory);
                    await File.WriteAllTextAsync(PreferencePath + ".tmp", JsonSerializer.Serialize(new { floatingEnabled = false }));
                    File.Move(PreferencePath + ".tmp", PreferencePath, overwrite: true);
                }
            }
            finally
            {
                process.Dispose();
                FloatingLock.Release();
                NotifyChanged();
            }
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or InvalidOperationException)
        {
            LastFloatingError = UiText.Get("悬浮统计卡未能启动或更新，请稍后重试。");
            Debug.WriteLine($"Usage card exit: {error.GetType().Name}");
            NotifyChanged();
        }
    }

    private static async Task SendFloatingSnapshotAsync()
    {
        Process? process = _ownedCard;
        if (process is null || _closing || !_cardReady || Interlocked.Exchange(ref _sendingSnapshot, 1) != 0) return;
        try { await WriteSnapshotAsync(process); }
        catch (Exception error) when (error is IOException or InvalidOperationException or OperationCanceledException)
        { Debug.WriteLine($"Usage card update: {error.GetType().Name}"); }
        finally { Interlocked.Exchange(ref _sendingSnapshot, 0); }
    }

    private static async Task WriteSnapshotAsync(Process process)
    {
        using var writeDeadline = CancellationTokenSource.CreateLinkedTokenSource(Lifetime.Token);
        writeDeadline.CancelAfter(TimeSpan.FromSeconds(2));
        await FloatingInputLock.WaitAsync(writeDeadline.Token);
        try
        {
            if (ReferenceEquals(_ownedCard, process) && !process.HasExited)
            {
                string command = JsonSerializer.Serialize(new { @event = "snapshot", snapshot = Display }, SnapshotJson);
                await process.StandardInput.WriteLineAsync(command.AsMemory(), writeDeadline.Token);
                await process.StandardInput.FlushAsync(writeDeadline.Token);
            }
        }
        finally { FloatingInputLock.Release(); }
    }

    private static void CloseFloatingInput()
    {
        // EOF closes only our inherited pipe; no process-name search or public control endpoint is used.
        // 关闭自有继承管道使子卡退出，不按进程名搜索，也不建立公开控制端点。
        if (_ownedCard is { } process)
        {
            CloseProcessInput(process);
            _ = CloseAfterPendingWriteAsync(process);
        }
    }

    private static async Task CloseAfterPendingWriteAsync(Process process)
    {
        try
        {
            using var closeDeadline = new CancellationTokenSource(TimeSpan.FromSeconds(2));
            await FloatingInputLock.WaitAsync(closeDeadline.Token);
            try { CloseProcessInput(process); }
            finally { FloatingInputLock.Release(); }
        }
        catch (Exception error) when (error is InvalidOperationException or OperationCanceledException) { }
    }

    private static void CloseProcessInput(Process process)
    {
        try { process.StandardInput.Close(); }
        catch (Exception error) when (error is IOException or InvalidOperationException) { }
    }

    private static bool IsProcessAlive(Process? process)
    {
        try { return process is { HasExited: false }; }
        catch (InvalidOperationException) { return false; }
    }

    private static async Task WaitForOwnedExitAsync(Process process, TimeSpan timeout)
    {
        Task exited = process.WaitForExitAsync();
        if (await Task.WhenAny(exited, Task.Delay(timeout)) == exited)
        {
            await exited;
            return;
        }
        // Only this exact Process handle belongs to us; never kill by name or affect the shared gateway.
        // 只结束此处持有的精确子进程，不按名称终止，也不影响共用模型网关。
        if (IsProcessAlive(process)) process.Kill();
        await exited.WaitAsync(TimeSpan.FromSeconds(2));
    }
}
