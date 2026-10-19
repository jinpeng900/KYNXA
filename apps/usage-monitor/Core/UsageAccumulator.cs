using System.Globalization;
using System.Numerics;
using System.Text.Json;

namespace Kynxa.UsageMonitor.Core;

public sealed record UsageSnapshot(
    BigInteger KnownTotalTokens,
    long Calls,
    long KnownTotalCalls,
    long KnownCacheCalls,
    BigInteger CacheHitTokens,
    BigInteger CacheInputTokens,
    bool HasLostRecords,
    long WarningCount,
    string LastWarning,
    long? LatestCallTokens,
    BigInteger KnownInputTokens,
    BigInteger KnownOutputTokens,
    long KnownInputCalls,
    long KnownOutputCalls)
{
    public bool HasUnknownTotals => Calls != KnownTotalCalls || HasLostRecords || WarningCount > 0;
    public bool HasCompleteCache => Calls > 0 && Calls == KnownCacheCalls && !HasLostRecords && WarningCount == 0;
    public string TokensText => KnownTotalTokens.ToString("N0", CultureInfo.InvariantCulture) + (HasUnknownTotals ? "+?" : "");
    public string CacheText => HasCompleteCache && CacheInputTokens > 0
        ? ((double)CacheHitTokens / (double)CacheInputTokens * 100).ToString("F1", CultureInfo.InvariantCulture) + "%"
        : "—";

    public string Describe(bool isReplay)
    {
        string mode = isReplay ? "演示 / 回放：历史日志统计，不代表本次实时消耗。" : "本次 KYNXA 运行后的模型调用统计。";
        string coverage = $"\n已记录 {Calls:N0} 次调用；完整用量 {KnownTotalCalls:N0}/{Calls:N0}；缓存字段 {KnownCacheCalls:N0}/{Calls:N0}。";
        string knownCache = CacheInputTokens > 0
            ? $"\n已知子集：命中 {CacheHitTokens:N0} / 输入 {CacheInputTokens:N0} token（{(double)CacheHitTokens / (double)CacheInputTokens * 100:F1}%）。"
            : "\n缓存命中率 = 已知缓存命中 token / 对应输入 token；当前没有可计算的分母。";
        string loss = HasLostRecords ? "\n采集曾丢失记录，真实总量和完整命中率未知。" : "";
        string warning = WarningCount > 0 ? $"\n采集警告 {WarningCount:N0} 次：{LastWarning}；总量可能不完整。" : "";
        return mode + coverage + knownCache + loss + warning;
    }
}

/// <summary>
/// Counts only terminal provider attempts; absent usage remains unknown, including failed attempts.
/// 仅统计模型调用终态；失败调用已有用量仍计入，缺少用量始终保持未知。
/// </summary>
public sealed class UsageAccumulator
{
    public const long MaximumSafeInteger = 9_007_199_254_740_991;
    public const int MaximumTrackedCallIds = 100_000;
    private readonly HashSet<string> _seenCallIds = new(StringComparer.Ordinal);
    private BigInteger _knownTotalTokens;
    private BigInteger _knownInputTokens;
    private BigInteger _knownOutputTokens;
    private BigInteger _cacheHitTokens;
    private BigInteger _cacheInputTokens;
    private long _calls;
    private long _knownTotalCalls;
    private long _knownInputCalls;
    private long _knownOutputCalls;
    private long _knownCacheCalls;
    private long _warningCount;
    private bool _hasLostRecords;
    private string _lastWarning = "";
    private long? _latestCallTokens;
    private bool _capacityWarningShown;

    public UsageSnapshot Snapshot => new(_knownTotalTokens, _calls, _knownTotalCalls, _knownCacheCalls,
        _cacheHitTokens, _cacheInputTokens, _hasLostRecords, _warningCount, _lastWarning, _latestCallTokens,
        _knownInputTokens, _knownOutputTokens, _knownInputCalls, _knownOutputCalls);

    public bool AcceptLine(string line)
    {
        try
        {
            using JsonDocument document = JsonDocument.Parse(line);
            JsonElement record = document.RootElement;
            if (record.ValueKind != JsonValueKind.Object)
            {
                Warn("日志行不是 JSON 对象");
                return false;
            }
            if (!record.TryGetProperty("event", out JsonElement eventName) || eventName.GetString() != "call_completed")
            {
                return false;
            }
            if (!record.TryGetProperty("schemaVersion", out JsonElement version) || !version.TryGetInt32(out int schemaVersion) || schemaVersion != 1)
            {
                Warn("遇到不支持的日志版本");
                return false;
            }
            if (!record.TryGetProperty("callId", out JsonElement callIdValue) || callIdValue.ValueKind != JsonValueKind.String ||
                string.IsNullOrWhiteSpace(callIdValue.GetString()))
            {
                Warn("调用缺少稳定标识，无法可靠去重");
                return false;
            }
            string callId = callIdValue.GetString()!;
            if (callId.Length > 128)
            {
                Warn("调用标识过长，无法可靠去重");
                return false;
            }
            if (_seenCallIds.Count >= MaximumTrackedCallIds && !_seenCallIds.Contains(callId))
            {
                // Retaining a bounded complete set is safer than evicting IDs and silently double-counting rotations.
                // 去重集合达到上限后停止新增统计，不能淘汰旧 ID 并在轮换时静默重复计费。
                if (!_capacityWarningShown)
                {
                    Warn("本次运行已达 100,000 次调用上限，请重启 KYNXA 开始新的统计");
                    _capacityWarningShown = true;
                }
                return false;
            }
            if (!_seenCallIds.Add(callId))
            {
                return false;
            }
            _calls++;
            long? inputTokens = ReadTokens(record, "inputTokens");
            long? outputTokens = ReadTokens(record, "outputTokens");
            long? totalTokens = ReadTokens(record, "totalTokens");
            long? hitTokens = ReadTokens(record, "cacheHitTokens");
            long? missTokens = ReadTokens(record, "cacheMissTokens");
            if (inputTokens.HasValue)
            {
                _knownInputTokens += inputTokens.Value;
                _knownInputCalls++;
            }
            if (outputTokens.HasValue)
            {
                _knownOutputTokens += outputTokens.Value;
                _knownOutputCalls++;
            }

            // A provider's contradictory fields must not become a plausible but fabricated total.
            // 供应商字段互相冲突时保留未知，不能展示看似可信的虚构总量。
            if (inputTokens.HasValue && outputTokens.HasValue)
            {
                long sumTokens = inputTokens.Value + outputTokens.Value;
                if (sumTokens > MaximumSafeInteger || (totalTokens.HasValue && totalTokens.Value != sumTokens))
                {
                    totalTokens = null;
                    Warn("输入、输出与总 token 字段冲突");
                }
                else
                {
                    totalTokens ??= sumTokens;
                }
            }
            _latestCallTokens = totalTokens;
            if (totalTokens.HasValue)
            {
                _knownTotalTokens += totalTokens.Value;
                _knownTotalCalls++;
            }
            bool cacheIsConsistent = inputTokens.HasValue && hitTokens.HasValue && hitTokens <= inputTokens;
            if (cacheIsConsistent && missTokens.HasValue && hitTokens!.Value + missTokens.Value != inputTokens!.Value)
            {
                cacheIsConsistent = false;
                Warn("缓存命中、未命中与输入 token 字段冲突");
            }
            if (cacheIsConsistent)
            {
                _cacheHitTokens += hitTokens!.Value;
                _cacheInputTokens += inputTokens!.Value;
                _knownCacheCalls++;
            }
            if (record.TryGetProperty("unrecordedCalls", out JsonElement unrecorded) &&
                unrecorded.ValueKind == JsonValueKind.Number && unrecorded.TryGetUInt64(out ulong lostCalls) && lostCalls > 0)
            {
                // This is a process-wide high-water mark, not a per-row loss count.
                // 此字段是进程级累计值，仅标记不完整，不能逐行重复累加丢失量。
                _hasLostRecords = true;
            }
            return true;
        }
        catch (JsonException)
        {
            Warn("忽略损坏的 JSON 日志行");
            return false;
        }
        catch (InvalidOperationException)
        {
            Warn("忽略字段类型错误的日志行");
            return false;
        }
    }

    public void Warn(string warning)
    {
        _warningCount++;
        _lastWarning = warning;
    }

    public void Reset()
    {
        // Retain IDs across reset so a rotation/replay cannot count a completed call twice.
        // 重置保留去重标识，避免文件轮换重新读取时把已完成调用再次计入。
        _knownTotalTokens = _knownInputTokens = _knownOutputTokens = _cacheHitTokens = _cacheInputTokens = BigInteger.Zero;
        _calls = _knownTotalCalls = _knownInputCalls = _knownOutputCalls = _knownCacheCalls = _warningCount = 0;
        // Preserve collection-integrity failures across a visual reset; reset cannot repair missing records.
        // 重置显示不能修复已丢失的采集记录，采集完整性状态继续保留。
        _lastWarning = "";
        _latestCallTokens = null;
        if (_capacityWarningShown) Warn("本次运行已达 100,000 次调用上限，请重启 KYNXA 开始新的统计");
    }

    private long? ReadTokens(JsonElement record, string fieldName)
    {
        if (!record.TryGetProperty(fieldName, out JsonElement value) || value.ValueKind == JsonValueKind.Null)
        {
            return null;
        }
        if (value.ValueKind == JsonValueKind.Number && value.TryGetInt64(out long tokens) && tokens is >= 0 and <= MaximumSafeInteger)
        {
            return tokens;
        }
        Warn("token 字段必须是非负安全整数");
        return null;
    }
}
