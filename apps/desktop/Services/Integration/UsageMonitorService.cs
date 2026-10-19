using System.Globalization;
using System.Net.Http;
using System.Numerics;
using System.Text.Json;
using Kynxa.UsageMonitor.Core;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Owns one run's usage projection for both the embedded bar and optional desktop card.
/// 内置统计条与可选桌面卡共用本次运行的唯一展示累计，不改写正式会话数据。
/// </summary>
public static partial class UsageMonitorService
{
    private static readonly UsageAccumulator Statistics = new();
    private static readonly object StatisticsLock = new();
    private static readonly CancellationTokenSource Lifetime = new();
    private static UsageSnapshot _snapshot = Statistics.Snapshot;
    private static DispatcherQueue? _dispatcher;
    private static bool _initialized;
    private static bool _closing;
    private static volatile bool _connected;
    private static volatile bool _hasCollectionGap;
    private static string? _collectionError;
    private static int _notificationQueued;
    public static event EventHandler? Changed;
    public static string DataDirectory => Path.Combine(StoragePaths.DesktopDirectory, "UsageMonitor");
    public static string DefaultUsageLogPath => Path.Combine(DataDirectory, "usage.jsonl");

    public static void Initialize(Window owner)
    {
        if (_initialized) return;
        _initialized = true;
        _dispatcher = owner.DispatcherQueue;
        LoadFloatingPreference();
        AppearanceService.Changed += PresentationChanged;
        UiText.LanguageChanged += PresentationChanged;
        owner.Closed += (_, _) =>
        {
            _closing = true;
            Lifetime.Cancel();
            AppearanceService.Changed -= PresentationChanged;
            UiText.LanguageChanged -= PresentationChanged;
            CloseFloatingInput();
        };
        // Prime the default EOF before gateway readiness so early new calls are not skipped.
        // 等待网关前记录默认日志尾部，避免漏掉窗口启动后最早完成的新调用。
        string configured = Environment.GetEnvironmentVariable("KYNXA_USAGE_LOG") ?? DefaultUsageLogPath;
        _ = MonitorAsync(configured, Lifetime.Token);
    }

    private static async Task MonitorAsync(string configuredPath, CancellationToken cancellationToken)
    {
        UsageLogTail? tail = null;
        string? logPath = null;
        try
        {
            if (Path.IsPathFullyQualified(configuredPath))
            {
                logPath = Path.GetFullPath(configuredPath);
                tail = new UsageLogTail(logPath, replay: false);
                await ReadTailAsync(tail, cancellationToken);
            }
            while (!cancellationToken.IsCancellationRequested)
            {
                try
                {
                    if (StoragePaths.IsMigrating)
                    {
                        await Task.Delay(250, cancellationToken);
                        continue;
                    }
                    string actualPath = await DiscoverUsageLogAsync(cancellationToken);
                    if (!string.Equals(logPath, actualPath, StringComparison.OrdinalIgnoreCase))
                    {
                        // A different source has no shared EOF checkpoint; completeness cannot be assumed.
                        // 不同来源没有共用的尾部检查点，切换后不能假定期间记录完整。
                        if (tail is not null) _hasCollectionGap = true;
                        logPath = actualPath;
                        tail = new UsageLogTail(actualPath, replay: false);
                        await ReadTailAsync(tail, cancellationToken);
                    }
                    _connected = true;
                    _collectionError = null;
                    NotifyChanged();
                    await RestoreFloatingAsync();
                    var healthDeadline = System.Diagnostics.Stopwatch.StartNew();
                    while (!cancellationToken.IsCancellationRequested)
                    {
                        if (StoragePaths.IsMigrating || healthDeadline.Elapsed >= TimeSpan.FromSeconds(5)) break;
                        await RefreshFloatingDirectoryAsync();
                        await ReadTailAsync(tail!, cancellationToken);
                        await Task.Delay(250, cancellationToken);
                    }
                }
                catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested) { break; }
                catch (Exception error) when (error is IOException or UnauthorizedAccessException or HttpRequestException
                    or JsonException or InvalidOperationException or ArgumentException or FormatException or OperationCanceledException)
                {
                    _connected = false;
                    _hasCollectionGap = true;
                    _collectionError = "用量采集暂不可用，正在重试连接。";
                    NotifyChanged();
                    await Task.Delay(TimeSpan.FromSeconds(5), cancellationToken);
                }
            }
        }
        catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested) { }
        catch (Exception error)
        {
            // Background failures remain visible without exposing response bodies or credentials.
            // 后台失败保留可见状态，不回显响应正文或凭据。
            System.Diagnostics.Debug.WriteLine($"Usage monitor: {error.GetType().Name}");
            _connected = false;
            _hasCollectionGap = true;
            _collectionError = "用量采集暂不可用，请重新打开 KYNXA。";
            NotifyChanged();
        }
    }

    private static async Task<string> DiscoverUsageLogAsync(CancellationToken cancellationToken)
    {
        Uri address = ModelGatewayService.Address;
        if (address.Scheme != "http" || address.Host is not ("127.0.0.1" or "localhost")) throw new InvalidOperationException();
        await ModelGatewayService.EnsureReadyAsync(cancellationToken);
        using var http = new HttpClient(new SocketsHttpHandler { UseProxy = false }) { Timeout = TimeSpan.FromSeconds(5) };
        using var response = await http.GetAsync(new Uri(address, "/health"), cancellationToken);
        response.EnsureSuccessStatusCode();
        using var document = JsonDocument.Parse(await response.Content.ReadAsStringAsync(cancellationToken));
        if (document.RootElement.ValueKind != JsonValueKind.Object
            || !document.RootElement.TryGetProperty("usageTelemetry", out var telemetry) || telemetry.ValueKind != JsonValueKind.Object
            || !telemetry.TryGetProperty("protocol", out var protocol) || protocol.ValueKind != JsonValueKind.Number
            || !protocol.TryGetInt32(out int version) || version != 1
            || !telemetry.TryGetProperty("enabled", out var active) || active.ValueKind != JsonValueKind.True
            || !telemetry.TryGetProperty("logPath", out var log) || log.ValueKind != JsonValueKind.String
            || log.GetString() is not string path || !Path.IsPathFullyQualified(path))
            throw new InvalidOperationException();
        return Path.GetFullPath(path);
    }

    private static async Task ReadTailAsync(UsageLogTail tail, CancellationToken cancellationToken)
    {
        bool changed = false;
        await tail.ReadAvailableAsync(line =>
        {
            lock (StatisticsLock)
            {
                long warningsBefore = Statistics.Snapshot.WarningCount;
                changed |= Statistics.AcceptLine(line) || Statistics.Snapshot.WarningCount != warningsBefore;
                _snapshot = Statistics.Snapshot;
            }
        }, warning =>
        {
            lock (StatisticsLock) { Statistics.Warn(warning); _snapshot = Statistics.Snapshot; }
            changed = true;
        }, cancellationToken);
        if (changed) NotifyChanged();
    }

    public static UsageDisplayState Display
    {
        get
        {
            UsageSnapshot snapshot;
            lock (StatisticsLock) snapshot = _snapshot;
            bool incompleteCoverage = _hasCollectionGap || !_connected || snapshot.HasLostRecords || snapshot.WarningCount > 0;
            bool unknown = snapshot.HasUnknownTotals || incompleteCoverage;
            bool hasUsage = _connected || snapshot.Calls > 0;
            string total = hasUsage ? Compact(snapshot.KnownTotalTokens) + (unknown ? "+?" : "") : "—";
            string input = Covered(snapshot.KnownInputTokens, snapshot.KnownInputCalls, snapshot.Calls, incompleteCoverage);
            string output = Covered(snapshot.KnownOutputTokens, snapshot.KnownOutputCalls, snapshot.Calls, incompleteCoverage);
            string hit = snapshot.KnownCacheCalls > 0 ? Number(snapshot.CacheHitTokens)
                + (snapshot.KnownCacheCalls < snapshot.Calls || incompleteCoverage ? "+?" : "") : "—";
            string description = UiText.Get("统计本次 KYNXA 启动后的模型用量；模型返回用量后更新。");
            description += "\n" + string.Format(CultureInfo.CurrentCulture, UiText.Get("已记录 {0} 次调用；完整用量 {1}/{0}；缓存字段 {2}/{0}。"),
                snapshot.Calls, snapshot.KnownTotalCalls, snapshot.KnownCacheCalls);
            if (snapshot.KnownCacheCalls > 0)
                description += "\n" + string.Format(CultureInfo.CurrentCulture, UiText.Get("已知缓存命中 {0} / 对应输入 {1} Token。"), Number(snapshot.CacheHitTokens), Number(snapshot.CacheInputTokens));
            if (unknown || snapshot.WarningCount > 0) description += "\n" + UiText.Get("部分用量未知，已知数值仅供参考。");
            if (_collectionError is not null) description += "\n" + UiText.Get(_collectionError);
            if (LastFloatingError is not null) description += "\n" + LastFloatingError;
            var palette = AppearanceService.Current;
            return new UsageDisplayState
            {
                TotalText = total, CacheText = hasUsage && !incompleteCoverage ? snapshot.CacheText : "—", InputText = hasUsage ? input : "—",
                OutputText = hasUsage ? output : "—", CacheHitText = hit, Description = description,
                IsIncomplete = unknown || snapshot.KnownCacheCalls < snapshot.Calls
                    || snapshot.KnownInputCalls < snapshot.Calls || snapshot.KnownOutputCalls < snapshot.Calls,
                ScopeLabel = UiText.Get("本次运行"), TotalLabel = UiText.Get("Token 消耗"),
                CacheLabel = UiText.Get("缓存命中率"), Accent = palette.Accent, Surface = palette.Main,
                Text = palette.Text, Secondary = palette.Secondary, Border = palette.Border,
                CollapseLabel = UiText.Get("折叠"), ExpandLabel = UiText.Get("展开"), ResetPositionLabel = UiText.Get("恢复右下角"),
                TopmostLabel = UiText.Get("置顶"), CloseLabel = UiText.Get("关闭")
            };
        }
    }

    private static string Covered(BigInteger value, long knownCalls, long calls, bool incompleteCoverage) => calls == 0 ? "—"
        : Number(value) + (knownCalls < calls || incompleteCoverage ? "+?" : "");
    private static string Number(BigInteger value) => value.ToString("N0", CultureInfo.InvariantCulture);
    private static string Compact(BigInteger value)
    {
        if (value < 10_000) return Number(value);
        BigInteger unit = value < 1_000_000 ? 1000 : value < 1_000_000_000 ? 1_000_000 : 1_000_000_000;
        string suffix = unit == 1000 ? "k" : unit == 1_000_000 ? "M" : "B";
        BigInteger tenths = (value * 10 + unit / 2) / unit;
        return (tenths / 10).ToString(CultureInfo.InvariantCulture) + "." + (tenths % 10).ToString(CultureInfo.InvariantCulture) + suffix;
    }

    private static void PresentationChanged(object? sender, EventArgs args) => NotifyChanged();
    private static void NotifyChanged()
    {
        if (_closing || _dispatcher is null || Interlocked.Exchange(ref _notificationQueued, 1) != 0) return;
        if (!_dispatcher.TryEnqueue(() =>
        {
            Interlocked.Exchange(ref _notificationQueued, 0);
            if (_closing) return;
            Changed?.Invoke(null, EventArgs.Empty);
            _ = SendFloatingSnapshotAsync();
        })) Interlocked.Exchange(ref _notificationQueued, 0);
    }
}
