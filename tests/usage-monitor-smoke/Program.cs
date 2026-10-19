using System.Numerics;
using System.Text;
using System.Text.Json;
using Kynxa.UsageMonitor.Core;

int passed = 0;
void Assert(bool condition, string name)
{
    if (!condition) throw new InvalidOperationException("FAIL: " + name);
    Console.WriteLine("PASS: " + name);
    passed++;
}
string Event(string id, long? input = 100, long? output = 20, long? hit = 50, long? total = 120, string status = "completed", ulong losses = 0)
{
    return JsonSerializer.Serialize(new { schemaVersion = 1, @event = "call_completed", callId = id,
        timestamp = "2026-10-15T00:00:00Z", providerId = "fixture", requestedModel = "fixture",
        inputTokens = input, outputTokens = output, totalTokens = total, cacheHitTokens = hit,
        cacheMissTokens = (long?)null, status, unrecordedCalls = losses });
}

UsageAccumulator usage = new();
usage.AcceptLine(Event("one"));
usage.AcceptLine(Event("two", 900, 30, 900, 930));
Assert(usage.Snapshot.KnownTotalTokens == 1050 && usage.Snapshot.CacheText == "95.0%", "weighted token cache rate, not request average");
Assert(usage.Snapshot.KnownInputTokens == 1000 && usage.Snapshot.KnownOutputTokens == 50 &&
    usage.Snapshot.KnownInputCalls == 2 && usage.Snapshot.KnownOutputCalls == 2, "input and output totals retain independent coverage");
Assert(!usage.AcceptLine(Event("one")) && usage.Snapshot.Calls == 2, "call ID deduplication");
usage.Reset();
Assert(!usage.AcceptLine(Event("two")) && usage.Snapshot.Calls == 0, "reset retains deduplication and does not recount old rows");
usage.AcceptLine(Event("failure", status: "error"));
Assert(usage.Snapshot.KnownTotalTokens == 120, "failed attempt with real usage remains charged");
usage.AcceptLine(Event("unknown", null, null, null, null, "missing_usage"));
Assert(usage.Snapshot.TokensText == "120+?" && usage.Snapshot.CacheText == "—" && usage.Snapshot.KnownTotalCalls == 1, "missing usage remains unknown with coverage");
UsageAccumulator partial = new();
partial.AcceptLine(Event("input-only", 0, null, null, null));
partial.AcceptLine(Event("output-only", null, 7, null, null));
Assert(partial.Snapshot.KnownInputTokens == 0 && partial.Snapshot.KnownInputCalls == 1 &&
    partial.Snapshot.KnownOutputTokens == 7 && partial.Snapshot.KnownOutputCalls == 1 &&
    partial.Snapshot.KnownTotalCalls == 0, "independent known zero input and partial output do not fabricate totals");
UsageAccumulator derived = new();
derived.AcceptLine(Event("derived", total: null));
Assert(derived.Snapshot.KnownTotalTokens == 120 && derived.Snapshot.TokensText == "120", "total derives only from both input and output");
derived.AcceptLine(Event("conflict", total: 121));
Assert(derived.Snapshot.TokensText == "120+?", "contradictory total is not accepted");
UsageAccumulator zero = new();
zero.AcceptLine(Event("zero", 0, 0, 0, 0));
Assert(zero.Snapshot.KnownTotalCalls == 1 && zero.Snapshot.KnownTotalTokens == BigInteger.Zero && zero.Snapshot.CacheText == "—", "real zero usage retained and zero denominator stays undefined");
zero.AcceptLine(Event("loss", losses: 2));
zero.AcceptLine(Event("loss2", losses: 2));
Assert(zero.Snapshot.HasLostRecords && zero.Snapshot.TokensText == "240+?" && zero.Snapshot.CacheText == "—", "cumulative lost records flags incomplete collection");
UsageAccumulator invalid = new();
invalid.AcceptLine("{broken");
invalid.AcceptLine(Event("unsafe").Replace("\"inputTokens\":100", "\"inputTokens\":9007199254740992"));
Assert(invalid.Snapshot.WarningCount == 2 && invalid.Snapshot.KnownCacheCalls == 0, "bad JSON and unsafe integers produce warnings without crashes");
UsageAccumulator cacheConflict = new();
cacheConflict.AcceptLine(Event("cache-conflict").Replace("\"cacheMissTokens\":null", "\"cacheMissTokens\":70"));
Assert(cacheConflict.Snapshot.CacheText == "—" && cacheConflict.Snapshot.WarningCount > 0, "inconsistent cache counts remain unknown");
UsageAccumulator bounded = new();
for (int index = 0; index < UsageAccumulator.MaximumTrackedCallIds; index++) bounded.AcceptLine(Event("bounded-" + index));
Assert(!bounded.AcceptLine(Event("beyond-bound")) && bounded.Snapshot.Calls == UsageAccumulator.MaximumTrackedCallIds &&
    bounded.Snapshot.HasUnknownTotals, "deduplication capacity is bounded and overflow stays explicitly incomplete");
Assert(!bounded.AcceptLine(Event("bounded-0")) && bounded.Snapshot.Calls == UsageAccumulator.MaximumTrackedCallIds,
    "deduplication capacity never evicts IDs and recounts old calls");
UsageControlReader control = new();
List<UsageDisplayState> displays = [];
int closed = 0;
int invalidCommands = 0;
void ReadControl(ReadOnlySpan<byte> bytes) => control.Append(bytes, displays.Add, () => closed++, () => invalidCommands++);
string snapshotCommand = JsonSerializer.Serialize(new { @event = "snapshot", snapshot = new UsageDisplayState
    { TotalText = "3,714", ScopeLabel = "本次运行", CacheText = "0.0%", Description = "已确认用量" } },
    new JsonSerializerOptions { PropertyNamingPolicy = JsonNamingPolicy.CamelCase, Encoder = System.Text.Encodings.Web.JavaScriptEncoder.UnsafeRelaxedJsonEscaping }) + "\n";
byte[] commandBytes = Encoding.UTF8.GetBytes(snapshotCommand);
int commandSplit = Array.IndexOf(commandBytes, (byte)0xE6) + 1;
ReadControl(commandBytes.AsSpan(0, commandSplit));
Assert(displays.Count == 0, "owner snapshot waits for full UTF-8 command line");
ReadControl(commandBytes.AsSpan(commandSplit));
Assert(displays.Single().TotalText == "3,714" && displays.Single().CacheText == "0.0%" &&
    displays.Single().ScopeLabel == "本次运行", "owner snapshot preserves formatted data and real zero cache rate");
ReadControl(Encoding.UTF8.GetBytes("{broken}\n{\"event\":\"snapshot\",\"snapshot\":{\"totalText\":null}}\n"));
Assert(invalidCommands == 2 && displays.Count == 1, "malformed snapshots and null UI fields are rejected without replacing last state");
ReadControl(new byte[UsageControlReader.MaximumCommandBytes + 1]);
ReadControl(Encoding.UTF8.GetBytes("\n" + snapshotCommand));
Assert(invalidCommands == 3 && displays.Count == 2, "owner command memory is bounded and resumes after oversized input");
ReadControl(Encoding.UTF8.GetBytes(snapshotCommand.Replace("#4568B2", "url(external)")));
Assert(invalidCommands == 4 && displays.Count == 2, "owner styling accepts only hex colors");
ReadControl(Encoding.UTF8.GetBytes(UsageControlReader.CloseCommand + "\r\n"));
Assert(closed == 1, "exact owner close command is accepted");
JsonLineBuffer assembler = new();
List<string> lines = [];
List<string> warnings = [];
byte[] utf8 = Encoding.UTF8.GetBytes("{\"label\":\"蓝发\"}\n");
int split = Array.IndexOf(utf8, (byte)0xE8) + 1;
assembler.Append(utf8.AsSpan(0, split), lines.Add, warnings.Add);
Assert(lines.Count == 0, "partial UTF-8 and JSON line remain buffered");
assembler.Append(utf8.AsSpan(split), lines.Add, warnings.Add);
Assert(lines.Single() == "{\"label\":\"蓝发\"}" && warnings.Count == 0, "UTF-8 byte boundaries decode correctly after newline");
assembler.Append(new byte[JsonLineBuffer.MaximumLineBytes + 1], lines.Add, warnings.Add);
assembler.Append(Encoding.UTF8.GetBytes("\n{}\n"), lines.Add, warnings.Add);
Assert(warnings.Count == 1 && lines.Last() == "{}", "oversized partial lines stay bounded and recover at next newline");

string temporaryRoot = Path.Combine(Path.GetTempPath(), "kynxa-usage-monitor-smoke-" + Guid.NewGuid().ToString("N"));
Directory.CreateDirectory(temporaryRoot);
try
{
    string path = Path.Combine(temporaryRoot, "usage.jsonl");
    await File.WriteAllTextAsync(path, Event("historical") + "\n", new UTF8Encoding(false));
    UsageLogTail tail = new(path, false);
    List<string> read = [];
    await tail.ReadAvailableAsync(read.Add, warnings.Add);
    Assert(read.Count == 0, "production tail starts at current EOF");
    string newLine = Event("new");
    await File.AppendAllTextAsync(path, newLine[..20], new UTF8Encoding(false));
    await tail.ReadAvailableAsync(read.Add, warnings.Add);
    Assert(read.Count == 0, "tail does not publish half lines");
    await File.AppendAllTextAsync(path, newLine[20..] + "\n", new UTF8Encoding(false));
    await tail.ReadAvailableAsync(read.Add, warnings.Add);
    Assert(read.Single() == newLine, "tail assembles incremental file writes");
    await File.WriteAllTextAsync(path, Event("truncated") + "\n", new UTF8Encoding(false));
    await tail.ReadAvailableAsync(read.Add, warnings.Add);
    Assert(read.Count == 2 && read.Last().Contains("truncated"), "file truncation resets byte stream");
    string rotatedPath = path + ".old";
    File.Move(path, rotatedPath);
    await File.WriteAllTextAsync(path, Event("replacement") + "\n", new UTF8Encoding(false));
    await tail.ReadAvailableAsync(read.Add, warnings.Add);
    Assert(read.Count == 3 && read.Last().Contains("replacement"), "same path file replacement detected");
    UsageLogTail replay = new(path, true);
    List<string> replayed = [];
    await replay.ReadAvailableAsync(replayed.Add, warnings.Add);
    Assert(replayed.Count == 1, "explicit replay starts from beginning");
    string missingPath = Path.Combine(temporaryRoot, "future.jsonl");
    UsageLogTail future = new(missingPath, false);
    List<string> futureRead = [];
    await future.ReadAvailableAsync(futureRead.Add, warnings.Add);
    await File.WriteAllTextAsync(missingPath, Event("future") + "\n", new UTF8Encoding(false));
    await future.ReadAvailableAsync(futureRead.Add, warnings.Add);
    Assert(futureRead.Count == 1, "absent log waits for creation and reads new events");
    string historicalPartialPath = Path.Combine(temporaryRoot, "partial.jsonl");
    await File.WriteAllTextAsync(historicalPartialPath, "{\"historical\":", new UTF8Encoding(false));
    UsageLogTail partialTail = new(historicalPartialPath, false);
    List<string> partialRead = [];
    await partialTail.ReadAvailableAsync(partialRead.Add, warnings.Add);
    await File.AppendAllTextAsync(historicalPartialPath, "true}\n" + Event("after-partial") + "\n", new UTF8Encoding(false));
    await partialTail.ReadAvailableAsync(partialRead.Add, warnings.Add);
    Assert(partialRead.Count == 1 && partialRead[0].Contains("after-partial"), "startup EOF inside historical partial line discards its continuation");
}
finally
{
    // Cleanup is limited to this freshly created fixture directory and never touches user usage logs.
    // 清理仅针对本测试新建的临时夹具目录，不触碰用户用量日志。
    Directory.Delete(temporaryRoot, true);
}
Console.WriteLine($"{passed} meaningful usage-monitor checks passed.");
