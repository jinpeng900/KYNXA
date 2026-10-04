using System.Diagnostics;
using System.Text.Json;
using System.Text.Json.Nodes;
using KYNXA_Desktop.Services;

// Explicit migration is an operator entry point. Its owner must first pause gateway
// writers under storage-migration.lock; this process does not double-acquire that lock.
if (args.Length > 0)
{
    if (args.Length != 6 || args[0] != "--source" || args[2] != "--target" || args[4] != "--pointer")
        throw new ArgumentException("Usage: --source <absolute> --target <empty-absolute> --pointer <absolute>");
    var migrated = await ExtensionStorageMigrationService.MoveAsync(args[1], args[3], args[5]);
    Console.WriteLine(JsonSerializer.Serialize(new { extensionRoot = migrated.ExtensionRoot, verifiedFiles = migrated.VerifiedFiles }));
    return;
}

int checks = 0;
void Check(bool condition, string name) { if (!condition) throw new Exception(name); checks++; }
async Task Reject(Func<Task> action, string name)
{
    try { await action(); }
    catch (Exception error) when (error is IOException or InvalidDataException or InvalidOperationException or OperationCanceledException) { checks++; return; }
    throw new Exception("Expected rejection: " + name);
}
string fixture = Path.Combine(Path.GetTempPath(), "kynxa-extensions-test-" + Guid.NewGuid().ToString("N"));
Directory.CreateDirectory(fixture);
async Task Write(string file, string contents)
{
    Directory.CreateDirectory(Path.GetDirectoryName(file)!);
    await File.WriteAllTextAsync(file, contents);
}
string source = Path.Combine(fixture, "Data"), target = Path.Combine(fixture, "Extensions"), pointer = Path.Combine(fixture, "profile", "extensions.json");
string external = Path.Combine(fixture, "external"), skillFile = Path.Combine(source, "Skills", "mine", "SKILL.md");
string externalSkill = Path.Combine(external, "SKILL.md");
string mcpSkill = Path.Combine(source, "MCP", "ExtraSkill", "SKILL.md"), agentSkill = Path.Combine(source, "Agent", "ExtraSkill", "SKILL.md");
await Write(skillFile, "---\nname: example\ndescription: fixture\n---\nHello");
await Write(mcpSkill, "---\nname: mcp-skill\ndescription: fixture\n---\nHello");
await Write(agentSkill, "---\nname: agent-skill\ndescription: fixture\n---\nHello");
await Write(externalSkill, "external stays put");
await Write(Path.Combine(source, "MCP", "npm-cache", "package", "server.js"), "not executed");
await Write(Path.Combine(source, "MCP", "browser-cache", "fixture.txt"), "cache");
await Write(Path.Combine(source, "Chats", "chat", "events.jsonl"), "private conversation stays in Data");
await Write(Path.Combine(source, "Models", "connections.json"), "not extension content");
Directory.CreateDirectory(Path.Combine(source, "Skills", "mine", "empty"));
string oldId = ExtensionConfigurationMigration.SkillId(skillFile), newId = ExtensionConfigurationMigration.SkillId(Path.Combine(target, "Skills", "mine", "SKILL.md"));
string externalId = ExtensionConfigurationMigration.SkillId(externalSkill), unknownId = new('a', 24);
string mcpAliasDirectory = Path.Combine(source, "mcp", "extraskill");
string mcpAliasId = ExtensionConfigurationMigration.SkillId(Path.Combine(mcpAliasDirectory, "SKILL.md"));
var config = new JsonObject
{
    ["version"] = 1, ["revision"] = 4,
    ["mcpServers"] = new JsonArray(new JsonObject
    {
        ["id"] = "owned", ["name"] = "Fixture", ["enabled"] = false,
        ["command"] = Path.Combine(source, "MCP", "bin", "node.exe"), ["cwd"] = Path.Combine(source, "MCP", "npm-cache"),
        ["args"] = new JsonArray(Path.Combine(source, "MCP", "npm-cache", "package", "server.js"), "--cache=" + Path.Combine(source, "MCP", "browser-cache"), externalSkill, "https://example.test/tool", "--headless"),
        ["envRefs"] = new JsonObject { ["API_KEY"] = "FICTIONAL_KEY_ENV" }, ["disabledTools"] = new JsonArray("write"),
        ["env"] = new JsonObject { ["npm_config_cache"] = Path.Combine(source, "MCP", "npm-cache"), ["PLAYWRIGHT_BROWSERS_PATH"] = Path.Combine(source, "MCP", "browser-cache"), ["EXTERNAL_PATH"] = external, ["TEXT"] = "--cache=" + Path.Combine(source, "MCP", "browser-cache") }
    }, new JsonObject { ["id"] = "external", ["command"] = "npx", ["args"] = new JsonArray("-y", "fixture@1.0"), ["cwd"] = external, ["enabled"] = true }),
    ["skillDirectories"] = new JsonArray(Path.Combine(source, "Skills"), external, Path.GetDirectoryName(agentSkill)!, mcpAliasDirectory),
    ["disabledSkills"] = new JsonArray(oldId, externalId, unknownId, ExtensionConfigurationMigration.SkillId(agentSkill), mcpAliasId),
    ["disabledOfficialMcpServers"] = new JsonArray("playwright"),
    ["officialMcpOverrides"] = new JsonArray(new JsonObject
    {
        ["presetId"] = "fetch", ["id"] = "official-fetch",
        ["changes"] = new JsonObject
        {
            ["command"] = Path.Combine(source, "MCP", "bin", "uvx.exe"), ["cwd"] = Path.Combine(source, "MCP"),
            ["args"] = new JsonArray("--config=" + Path.Combine(source, "MCP", "settings.json"), externalSkill),
            ["enabled"] = true, ["envRefs"] = new JsonObject { ["API_TOKEN"] = "SYNTHETIC_TOKEN_ENV" }
        }
    })
};
string configPath = Path.Combine(source, "Agent", "config.json"), originalConfig = config.ToJsonString();
await Write(configPath, originalConfig);
Check(ExtensionPaths.Resolve(source, null, pointer).Root == source, "formal Data fallback");
Check(ExtensionPaths.Resolve(source, null, pointer).UsesLegacyRoot, "fallback identity");
await Write(pointer, JsonSerializer.Serialize(new { version = 1, extensionRoot = source }));
Check(!ExtensionPaths.Resolve(source, null, pointer).UsesLegacyRoot, "explicit root identity");
Check(ExtensionPaths.Resolve(source, target, pointer).Root == target, "env precedence");
Check(ExtensionPaths.Resolve(source, null, pointer, ignorePointer: true).UsesLegacyRoot, "isolated Data ignores pointer");
string originalPointer = File.ReadAllText(pointer);
var result = await ExtensionStorageMigrationService.MoveAsync(source, target, pointer);
Check(result.ExtensionRoot == target && result.VerifiedFiles == 6, "extension-only files verified");
Check(!Directory.Exists(Path.Combine(target, "Chats")) && !Directory.Exists(Path.Combine(target, "Models")), "conversations and credentials excluded");
Check(File.ReadAllText(configPath) == originalConfig && File.ReadAllText(skillFile).Contains("Hello"), "source retained");
Check(File.ReadAllText(externalSkill) == "external stays put", "external files retained");
Check(Directory.Exists(Path.Combine(target, "Skills", "mine", "empty")), "empty directory retained");
var copiedConfig = JsonNode.Parse(File.ReadAllText(Path.Combine(target, "Agent", "config.json")))!;
Check(copiedConfig["revision"]!.GetValue<int>() == 5, "revision increment");
Check(copiedConfig["mcpServers"]![0]!["command"]!.GetValue<string>() == Path.Combine(target, "MCP", "bin", "node.exe"), "owned command remapped");
Check(copiedConfig["mcpServers"]![0]!["cwd"]!.GetValue<string>() == Path.Combine(target, "MCP", "npm-cache"), "owned cwd remapped");
Check(copiedConfig["mcpServers"]![0]!["args"]![0]!.GetValue<string>().StartsWith(target), "owned absolute argument remapped");
Check(copiedConfig["mcpServers"]![0]!["args"]![1]!.GetValue<string>() == "--cache=" + Path.Combine(target, "MCP", "browser-cache"), "owned flag path remapped");
Check(copiedConfig["mcpServers"]![0]!["args"]![2]!.GetValue<string>() == externalSkill, "external argument retained");
Check(copiedConfig["mcpServers"]![1]!["command"]!.GetValue<string>() == "npx" && copiedConfig["mcpServers"]![1]!["cwd"]!.GetValue<string>() == external, "external executable retained");
Check(copiedConfig["mcpServers"]![0]!["enabled"]!.GetValue<bool>() == false && copiedConfig["mcpServers"]![0]!["disabledTools"]![0]!.GetValue<string>() == "write", "tool enable flags retained");
Check(copiedConfig["skillDirectories"]![0]!.GetValue<string>() == Path.Combine(target, "Skills") && copiedConfig["skillDirectories"]![1]!.GetValue<string>() == external, "skill directories remapped selectively");
Check(copiedConfig["disabledSkills"]![0]!.GetValue<string>() == newId && copiedConfig["disabledSkills"]![1]!.GetValue<string>() == externalId && copiedConfig["disabledSkills"]![2]!.GetValue<string>() == unknownId, "disabled skill identities retained");
Check(copiedConfig["disabledSkills"]![3]!.GetValue<string>() == ExtensionConfigurationMigration.SkillId(Path.Combine(target, "Agent", "ExtraSkill", "SKILL.md")), "Agent-owned disabled ID remapped");
Check(copiedConfig["disabledSkills"]![4]!.GetValue<string>() == ExtensionConfigurationMigration.SkillId(Path.Combine(target, "mcp", "extraskill", "SKILL.md")), "configured casing alias disabled ID remapped");
Check(copiedConfig["mcpServers"]![0]!["env"]!["npm_config_cache"]!.GetValue<string>() == Path.Combine(target, "MCP", "npm-cache") && copiedConfig["mcpServers"]![0]!["env"]!["PLAYWRIGHT_BROWSERS_PATH"]!.GetValue<string>() == Path.Combine(target, "MCP", "browser-cache"), "owned env cache paths remapped");
Check(copiedConfig["mcpServers"]![0]!["envRefs"]!["API_KEY"]!.GetValue<string>() == "FICTIONAL_KEY_ENV" && copiedConfig["mcpServers"]![0]!["env"]!["EXTERNAL_PATH"]!.GetValue<string>() == external && copiedConfig["mcpServers"]![0]!["env"]!["TEXT"]!.GetValue<string>().StartsWith("--cache=" + source), "env references external paths and embedded text retained");
Check(File.ReadAllText(Path.Combine(target, "extensions-pointer.previous.json")) == originalPointer, "pointer backup");
Check(ExtensionPaths.Resolve(source, null, pointer).Root == target, "pointer activated last");
var officialChanges = copiedConfig["officialMcpOverrides"]![0]!["changes"]!;
Check(officialChanges["command"]!.GetValue<string>() == Path.Combine(target, "MCP", "bin", "uvx.exe") &&
    officialChanges["cwd"]!.GetValue<string>() == Path.Combine(target, "MCP"), "official user override runtime paths relocated");
Check(officialChanges["args"]![0]!.GetValue<string>() == "--config=" + Path.Combine(target, "MCP", "settings.json") &&
    officialChanges["args"]![1]!.GetValue<string>() == externalSkill, "official user override owned arguments relocate; external paths remain");
Check(officialChanges["enabled"]!.GetValue<bool>() && officialChanges["envRefs"]!["API_TOKEN"]!.GetValue<string>() == "SYNTHETIC_TOKEN_ENV" &&
    copiedConfig["disabledOfficialMcpServers"]![0]!.GetValue<string>() == "playwright", "official choices and credential references survive user migration");

foreach (string name in ExtensionPaths.LayoutDirectories)
    Check(Directory.Exists(Path.Combine(target, name)), "complete first migration framework: " + name);
Check(ExtensionPaths.ValidateLayout(target), "new layout metadata version valid");
await Write(Path.Combine(target, "Backups", "Extensions", "previous", "config.json"), "synthetic recovery config");
Directory.CreateDirectory(Path.Combine(target, "Backups", "Extensions", "previous", "empty"));
await Write(Path.Combine(target, "Backups", "formal-chat-backup.json"), "formal Data backup must remain outside extensions");
await Write(Path.Combine(target, "user-note.txt"), "not app owned");
await Write(Path.Combine(target, "extension-layout.json"), "{\"version\":1,\"extra\":\"preserved\"}");
string remigrated = Path.Combine(fixture, "Extensions-remigrated");
await ExtensionStorageMigrationService.MoveAsync(target, remigrated, pointer);
Check(File.ReadAllText(Path.Combine(remigrated, "Backups", "Extensions", "previous", "config.json")) == "synthetic recovery config", "extension recovery files migrate");
Check(Directory.Exists(Path.Combine(remigrated, "Backups", "Extensions", "previous", "empty")), "recovery empty directory migrates");
Check(!File.Exists(Path.Combine(remigrated, "Backups", "formal-chat-backup.json")) && !File.Exists(Path.Combine(remigrated, "user-note.txt")), "shared Data backup and user root files excluded");
Check(File.ReadAllText(Path.Combine(remigrated, "extension-layout.json")) == "{\"version\":1,\"extra\":\"preserved\"}", "layout unknown metadata retained");
string[] history = Directory.GetFiles(Path.Combine(remigrated, "Backups", "Extensions", "Migrations"), "*.json", SearchOption.AllDirectories);
Check(history.Any(path => Path.GetFileName(path) == "extension-migration-info.json") && history.Any(path => Path.GetFileName(path) == "extensions-pointer.previous.json"), "prior migration metadata retained in recovery namespace");
var remigratedConfig = JsonNode.Parse(File.ReadAllText(Path.Combine(remigrated, "Agent", "config.json")))!;
Check(remigratedConfig["disabledSkills"]![0]!.GetValue<string>() == ExtensionConfigurationMigration.SkillId(Path.Combine(remigrated, "Skills", "mine", "SKILL.md")), "disabled skill remains disabled after repeated migration");
Check(remigratedConfig["officialMcpOverrides"]![0]!["changes"]!["command"]!.GetValue<string>() == Path.Combine(remigrated, "MCP", "bin", "uvx.exe"),
    "official user override path survives repeated migration");
Check(File.ReadAllText(Path.Combine(target, "Backups", "formal-chat-backup.json")).Contains("formal Data"), "shared backup source retained");

string initial = Path.Combine(fixture, "first-inactive-layout");
ExtensionPaths.EnsureLayout(initial);
Check(!File.Exists(Path.Combine(initial, "Agent", "config.json")), "initialization never invents settings or a second configuration");
string initialMetadata = File.ReadAllText(Path.Combine(initial, "extension-layout.json"));
ExtensionPaths.EnsureLayout(initial);
Check(File.ReadAllText(Path.Combine(initial, "extension-layout.json")) == initialMetadata, "initialization idempotent");
await Write(Path.Combine(initial, "extension-layout.json"), "{\"version\":99}");
string unsupportedTarget = Path.Combine(fixture, "unsupported-layout-target");
await Reject(() => ExtensionStorageMigrationService.MoveAsync(initial, unsupportedTarget, Path.Combine(fixture, "new-profile", "extensions.json")), "future layout rejected before copying");
Check(!Directory.Exists(unsupportedTarget) && File.ReadAllText(Path.Combine(initial, "extension-layout.json")) == "{\"version\":99}", "future layout source retained");
// Restore only this synthetic pointer so the existing conflict checks still use their original source.
await Write(pointer, JsonSerializer.Serialize(new { version = 1, extensionRoot = target }));

await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, target, pointer), "nonempty target");
await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, Path.Combine(source, "nested"), pointer), "nested target");
await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, fixture, pointer), "ancestor target");
await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, "relative", pointer), "relative target");
await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, Path.Combine(fixture, "relative-pointer"), "relative.json"), "relative pointer");
await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, Path.GetPathRoot(source)!, pointer), "volume root");
await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, Path.Combine(fixture, "stale"), pointer), "active source conflict");

string failurePointer = Path.Combine(fixture, "failure-profile", "extensions.json");
await Write(failurePointer, JsonSerializer.Serialize(new { version = 1, extensionRoot = source }));
string untouchedPointer = File.ReadAllText(failurePointer);
using (var cancellation = new CancellationTokenSource())
{
    cancellation.Cancel();
    await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, Path.Combine(fixture, "cancelled"), failurePointer, cancellationToken: cancellation.Token), "pre-cancelled");
}
using (var cancellation = new CancellationTokenSource())
    await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, Path.Combine(fixture, "cancel-before-commit"), failurePointer,
        new ImmediateProgress(value => { if (value.StartsWith("正在核对")) cancellation.Cancel(); }), cancellation.Token), "cancel before activation");
Check(File.ReadAllText(failurePointer) == untouchedPointer, "cancellation preserves pointer");
await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, Path.Combine(fixture, "concurrent-source"), failurePointer,
    new ImmediateProgress(value => { if (value.StartsWith("正在核对")) File.AppendAllText(configPath, " "); })), "source changed");
await Write(configPath, originalConfig);
await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, Path.Combine(fixture, "concurrent-pointer"), failurePointer,
    new ImmediateProgress(value => { if (value.StartsWith("正在核对")) File.AppendAllText(failurePointer, " "); })), "pointer changed");
await Write(failurePointer, untouchedPointer);
string tampered = Path.Combine(fixture, "tampered-destination");
await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, tampered, failurePointer,
    new ImmediateProgress(value => { if (value.StartsWith("正在核对")) File.AppendAllText(Path.Combine(tampered, "MCP", "browser-cache", "fixture.txt"), "changed"); })), "copied payload modified");
string lostDirectory = Path.Combine(fixture, "missing-empty-destination");
await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, lostDirectory, failurePointer,
    new ImmediateProgress(value => { if (value.StartsWith("正在核对")) Directory.Delete(Path.Combine(lostDirectory, "Skills", "mine", "empty")); })), "missing copied empty directory");
string injectedTarget = Path.Combine(fixture, "injected-destination");
await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, injectedTarget, failurePointer,
    new ImmediateProgress(value => { if (value.StartsWith("正在核对")) File.WriteAllText(Path.Combine(injectedTarget, "unexpected.txt"), "injected"); })), "unexpected destination files");
Check(File.ReadAllText(failurePointer) == untouchedPointer, "directory and destination mutation preserve active pointer");
foreach (string invalid in new[] { "{", "null", "{\"version\":2,\"extensionRoot\":\"ignored\"}", "{\"version\":\"1\",\"extensionRoot\":\"ignored\"}", "{\"version\":1,\"extensionRoot\":\"relative\"}", "{\"version\":1,\"version\":1,\"extensionRoot\":\"ignored\"}" })
{
    await Write(failurePointer, invalid);
    string invalidTarget = Path.Combine(fixture, "invalid-pointer-" + Guid.NewGuid().ToString("N"));
    await Reject(() => Task.FromResult(ExtensionPaths.Resolve(source, null, failurePointer)), "resolver rejects unsupported pointer");
    await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, invalidTarget, failurePointer), "migration rejects unsupported pointer");
    Check(!Directory.Exists(invalidTarget) && File.ReadAllText(failurePointer) == invalid, "invalid pointer retained");
}
await Write(failurePointer, untouchedPointer);
foreach (string invalid in new[] { "{", "{\"version\":2}", "{\"version\":1,\"revision\":-1,\"mcpServers\":[],\"skillDirectories\":[]}" })
{
    await Write(configPath, invalid);
    await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, Path.Combine(fixture, "invalid-config-" + Guid.NewGuid().ToString("N")), failurePointer), "bad agent config");
}
await Write(configPath, originalConfig);
await File.WriteAllBytesAsync(configPath, [0x7B, 0x22, 0xFF, 0x22, 0x7D]);
await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, Path.Combine(fixture, "invalid-utf8"), failurePointer), "invalid UTF8 config");
await Write(configPath, originalConfig);
await using (var operationLock = new FileStream(Path.Combine(Path.GetDirectoryName(failurePointer)!, "extensions-operation.lock"), FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None))
    await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, Path.Combine(fixture, "concurrent-migration"), failurePointer), "concurrent migration lock");
if (OperatingSystem.IsWindows())
{
    string junction = Path.Combine(source, "Skills", "link");
    var start = new ProcessStartInfo("cmd.exe") { UseShellExecute = false, CreateNoWindow = true, RedirectStandardOutput = true, RedirectStandardError = true };
    foreach (string argument in new[] { "/c", "mklink", "/J", junction, external }) start.ArgumentList.Add(argument);
    using var process = Process.Start(start)!;
    await process.WaitForExitAsync();
    Check(process.ExitCode == 0, "junction fixture created");
    await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, Path.Combine(fixture, "linked-source"), failurePointer), "source junction");
    await Reject(() => Task.FromResult(ExtensionPaths.Resolve(junction, null, Path.Combine(fixture, "absent.json"))), "resolver junction");
    Directory.Delete(junction); // Remove only the generated junction, never its destination.
    string hardlink = Path.Combine(source, "Skills", "mine", "linked.txt");
    var hardlinkStart = new ProcessStartInfo("cmd.exe") { UseShellExecute = false, CreateNoWindow = true, RedirectStandardOutput = true, RedirectStandardError = true };
    foreach (string argument in new[] { "/c", "mklink", "/H", hardlink, externalSkill }) hardlinkStart.ArgumentList.Add(argument);
    using var hardlinkProcess = Process.Start(hardlinkStart)!;
    await hardlinkProcess.WaitForExitAsync();
    Check(hardlinkProcess.ExitCode == 0, "hardlink fixture created");
    await Reject(() => ExtensionStorageMigrationService.MoveAsync(source, Path.Combine(fixture, "hardlinked-source"), failurePointer), "source hardlink");
    File.Delete(hardlink); // Delete only this generated link; the external file remains intact.
    Check(File.ReadAllText(externalSkill) == "external stays put", "hardlink target retained");
}

// Existing Data migration keeps explicit extensions fixed, and moves fallback extensions coherently.
string legacyDesktop = Path.Combine(source, "Desktop"), legacyModels = Path.Combine(source, "Models");
Directory.CreateDirectory(legacyDesktop);
string dataPointer = Path.Combine(fixture, "data-profile", "storage.json");
Directory.CreateDirectory(Path.GetDirectoryName(dataPointer)!);
string dataTarget = Path.Combine(fixture, "Data-migrated");
await StorageMigrationService.MoveAsync(legacyDesktop, legacyModels, dataTarget, dataPointer);
var dataConfig = JsonNode.Parse(File.ReadAllText(Path.Combine(dataTarget, "Agent", "config.json")))!;
Check(dataConfig["mcpServers"]![0]!["cwd"]!.GetValue<string>() == Path.Combine(dataTarget, "MCP", "npm-cache"), "Data fallback rewrites owned config");
Check(dataConfig["disabledSkills"]![0]!.GetValue<string>() == ExtensionConfigurationMigration.SkillId(Path.Combine(dataTarget, "Skills", "mine", "SKILL.md")), "Data fallback remaps disabled IDs");
Check(File.Exists(Path.Combine(dataTarget, "MCP", "browser-cache", "fixture.txt")), "Data fallback moves cache");
string fixedTarget = Path.Combine(fixture, "Data-explicit-extension");
await StorageMigrationService.MoveAsync(legacyDesktop, legacyModels, fixedTarget, dataPointer, migrateExtensions: false);
Check(!Directory.Exists(Path.Combine(fixedTarget, "Agent")) && !Directory.Exists(Path.Combine(fixedTarget, "Skills")) && !Directory.Exists(Path.Combine(fixedTarget, "MCP")), "explicit extension roots excluded from Data migration");
Check(ExtensionPaths.Resolve(source, null, pointer).Root == target, "Data migration does not alter extensions pointer");

// Runtime metadata moves without launching Python, uv or any downloaded package.
// The interpreter is an inert file so this test verifies ownership and relocation,
// not third-party console-launcher portability or a live Python installation.
string runtimeSource = Path.Combine(fixture, "runtime-source"), runtimeTarget = Path.Combine(fixture, "runtime-target");
string runtimePointer = Path.Combine(fixture, "runtime-profile", "extensions.json");
string interpreterDirectory = Path.Combine(runtimeSource, "MCP", "python", "cpython-3.12.15-windows-x86_64-none");
string interpreter = Path.Combine(interpreterDirectory, "python.exe");
string toolEnvironment = Path.Combine(runtimeSource, "MCP", "uv-tools", "fixture");
string cacheEnvironment = Path.Combine(runtimeSource, "MCP", "uv-cache", "archive-v0", "fixture");
string entrypoint = Path.Combine(runtimeSource, "MCP", "bin", "fixture.exe");
string runtimeConfig = $"home = {interpreterDirectory}\r\nexecutable = {interpreter}\r\nversion_info = 3.12.15\r\ncustom = {interpreter}\r\n# executable = {interpreter}\r\n";
await Write(interpreter, "NOT-AN-EXECUTABLE: owned interpreter fixture");
await Write(entrypoint, "NOT-AN-EXECUTABLE: unchanged entrypoint fixture");
await Write(Path.Combine(toolEnvironment, "pyvenv.cfg"), runtimeConfig);
await Write(Path.Combine(cacheEnvironment, "pyvenv.cfg"), runtimeConfig);
string externalRuntimeConfig = $"home = {external}\nexecutable = {externalSkill}\n";
await Write(Path.Combine(runtimeSource, "MCP", "uv-tools", "external", "pyvenv.cfg"), externalRuntimeConfig);
await Write(Path.Combine(runtimeSource, "Skills", "unrelated", "pyvenv.cfg"), runtimeConfig);
string TomlPath(string path) => JsonSerializer.Serialize(path.Replace('\\', '/'));
string runtimeReceipt = $"[tool]\npython = {TomlPath(interpreter)} # pinned interpreter\nrequirements = [{{ name = \"fixture\", specifier = \"==1.0\", description = {TomlPath(interpreter)} }}]\nentrypoints = [\n    {{ name = \"fixture\", install-path = {TomlPath(entrypoint)}, from = \"fixture\" }},\n    {{ name = \"external\", install-path = {TomlPath(externalSkill)} }},\n]\n# install-path = {TomlPath(entrypoint)}\n[tool.options]\npython = {TomlPath(interpreter)}\n";
await Write(Path.Combine(toolEnvironment, "uv-receipt.toml"), runtimeReceipt);
await Write(runtimePointer, JsonSerializer.Serialize(new { version = 1, extensionRoot = runtimeSource }));
await ExtensionStorageMigrationService.MoveAsync(runtimeSource, runtimeTarget, runtimePointer);
string targetInterpreter = Path.Combine(runtimeTarget, "MCP", "python", "cpython-3.12.15-windows-x86_64-none", "python.exe");
string targetInterpreterDirectory = Path.GetDirectoryName(targetInterpreter)!;
foreach (string environment in new[] { Path.Combine(runtimeTarget, "MCP", "uv-tools", "fixture"), Path.Combine(runtimeTarget, "MCP", "uv-cache", "archive-v0", "fixture") })
{
    string copied = File.ReadAllText(Path.Combine(environment, "pyvenv.cfg"));
    Check(copied.Contains("home = " + targetInterpreterDirectory + "\r\n") && copied.Contains("executable = " + targetInterpreter + "\r\n"), "owned UV interpreter fields relocated");
    Check(copied.Contains("custom = " + interpreter + "\r\n") && copied.Contains("# executable = " + interpreter + "\r\n"), "non-runtime cfg fields and comments retained");
}
Check(File.ReadAllText(Path.Combine(toolEnvironment, "pyvenv.cfg")) == runtimeConfig && File.ReadAllText(Path.Combine(cacheEnvironment, "pyvenv.cfg")) == runtimeConfig, "original UV environment metadata unchanged");
Check(File.ReadAllText(Path.Combine(toolEnvironment, "uv-receipt.toml")) == runtimeReceipt, "original UV receipt unchanged");
Check(File.ReadAllText(Path.Combine(runtimeTarget, "MCP", "uv-tools", "external", "pyvenv.cfg")) == externalRuntimeConfig, "external runtime paths preserved without reads");
Check(File.ReadAllText(Path.Combine(runtimeTarget, "Skills", "unrelated", "pyvenv.cfg")) == runtimeConfig, "runtime-named files outside UV roots unchanged");
string targetReceipt = File.ReadAllText(Path.Combine(runtimeTarget, "MCP", "uv-tools", "fixture", "uv-receipt.toml"));
Check(targetReceipt.Contains("python = " + TomlPath(targetInterpreter) + " # pinned interpreter"), "UV requested interpreter relocated");
Check(targetReceipt.Contains("install-path = " + TomlPath(Path.Combine(runtimeTarget, "MCP", "bin", "fixture.exe"))), "UV entrypoint destination relocated");
Check(targetReceipt.Contains("install-path = " + TomlPath(externalSkill)), "UV external entrypoint retained");
Check(targetReceipt.Contains("description = " + TomlPath(interpreter)) && targetReceipt.Contains("# install-path = " + TomlPath(entrypoint)) && targetReceipt.Contains("[tool.options]\npython = " + TomlPath(interpreter)), "receipt requirements comments and options unchanged");
Check(!File.Exists(Path.Combine(runtimeTarget, "Agent", "config.json")), "runtime relocation does not create Agent config");
// Move only this generated source aside; no old-root file may satisfy the new paths.
Directory.Move(runtimeSource, runtimeSource + "-retained");
Check(!Directory.Exists(runtimeSource) && Directory.Exists(targetInterpreterDirectory) && File.ReadAllText(targetInterpreter).Contains("owned interpreter fixture"), "new-root interpreter exists after old root is removed");
Check(File.ReadAllText(Path.Combine(runtimeTarget, "MCP", "bin", "fixture.exe")) == "NOT-AN-EXECUTABLE: unchanged entrypoint fixture", "entrypoint binary never patched or executed");
string runtimeAgain = Path.Combine(fixture, "runtime-again");
await ExtensionStorageMigrationService.MoveAsync(runtimeTarget, runtimeAgain, runtimePointer);
Check(File.ReadAllText(Path.Combine(runtimeAgain, "MCP", "uv-tools", "fixture", "pyvenv.cfg")).Contains("executable = " + Path.Combine(runtimeAgain, "MCP", "python", "cpython-3.12.15-windows-x86_64-none", "python.exe")), "repeat UV runtime relocation works");

string invalidRuntime = Path.Combine(fixture, "invalid-runtime"), invalidRuntimePointer = Path.Combine(fixture, "invalid-runtime-profile", "extensions.json");
await Write(Path.Combine(invalidRuntime, "MCP", "uv-cache", "venv", "pyvenv.cfg"), "executable = " + Path.Combine(invalidRuntime, "MCP", "python", "missing.exe") + "\n");
await Write(invalidRuntimePointer, JsonSerializer.Serialize(new { version = 1, extensionRoot = invalidRuntime }));
string savedRuntimePointer = File.ReadAllText(invalidRuntimePointer);
await Reject(() => ExtensionStorageMigrationService.MoveAsync(invalidRuntime, Path.Combine(fixture, "invalid-runtime-target"), invalidRuntimePointer), "missing owned interpreter cannot activate target");
Check(File.ReadAllText(invalidRuntimePointer) == savedRuntimePointer && File.ReadAllText(Path.Combine(invalidRuntime, "MCP", "uv-cache", "venv", "pyvenv.cfg")).Contains("missing.exe"), "failed runtime relocation preserves pointer and source");
Console.WriteLine($"PASS: {checks} checks; isolated resolver, selective copy, hashes, paths, IDs, versions, cancellation, conflicts and Data fallback.");
Console.WriteLine("Fixture: " + fixture);

sealed class ImmediateProgress(Action<string> action) : IProgress<string> { public void Report(string value) => action(value); }
