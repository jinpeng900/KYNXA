using System.Text.Json.Nodes;
using KYNXA_Desktop.Services;

static void Check(bool condition, string name) { if (!condition) throw new Exception(name); }
static async Task Reject(Func<Task> action, string name)
{
    try { await action(); } catch (Exception e) when (e is IOException or InvalidDataException or InvalidOperationException or OperationCanceledException) { return; }
    throw new Exception("Expected rejection: " + name);
}
var root = Path.Combine(Path.GetTempPath(), "kynxa-storage-test-" + Guid.NewGuid().ToString("N"));
var desktop = Path.Combine(root, "old", "Desktop");
var models = Path.Combine(root, "old", "Models");
var pointer = Path.Combine(root, "profile", "storage.json");
Directory.CreateDirectory(Path.Combine(desktop, "Projects", "one", "empty"));
Directory.CreateDirectory(Path.Combine(models, "sessions"));
Directory.CreateDirectory(Path.GetDirectoryName(pointer)!);
string managed = Path.Combine(desktop, "Projects", "one"), external = Path.Combine(root, "external-work");
Directory.CreateDirectory(external);
await File.WriteAllTextAsync(Path.Combine(external, "untouched.txt"), "keep");
await File.WriteAllTextAsync(Path.Combine(desktop, "projects.json"), System.Text.Json.JsonSerializer.Serialize(new[] {
    new { Id = "one", FolderPath = managed, Chats = new[] { "chat-one" } }, new { Id = "two", FolderPath = external, Chats = Array.Empty<string>() } }));
await File.WriteAllTextAsync(Path.Combine(desktop, "chats.json"), "[\"你好\"]");
await File.WriteAllTextAsync(Path.Combine(models, "connections.json"), "{\"providers\":[{\"apiKey\":\"test-only\"}]}");
await File.WriteAllTextAsync(Path.Combine(models, "sessions", "one.json"), "[\"context\"]");
string oldRoot = Path.GetDirectoryName(models)!;
string catalog = System.Text.Json.JsonSerializer.Serialize(new { Version = 1, Projects = new[] {
    new { Id = "one", FolderPath = managed }, new { Id = "two", FolderPath = external } }, Chats = Array.Empty<string>() });
await File.WriteAllTextAsync(Path.Combine(oldRoot, "catalog.json"), catalog);
await File.WriteAllTextAsync(Path.Combine(oldRoot, ".catalog-transaction.json"), "{\"catalog\":" + catalog + "}");
await File.WriteAllTextAsync(Path.Combine(oldRoot, ".conversations-v1.json"), "{\"version\":1}");
string settings = "{\"Appearance\":{\"Theme\":\"system\"},\"Storage\":{\"LayoutVersion\":1,\"StoreId\":\"stable-store\",\"CreatedAt\":\"2026-01-01T00:00:00.000Z\"}}";
await File.WriteAllTextAsync(Path.Combine(oldRoot, "settings.json"), settings);
string[] canonicalFiles = ["Projects/one/Sessions/chat-one/events.jsonl", "Chats/chat-two/events.jsonl", "Trash/chat-three/events.jsonl", "Backups/conversations-v1/Desktop/projects.json",
    "Projects/one/Sessions/chat-one/attachments/drawing.txt", "Projects/one/Memory/preferences.md", "Chats/chat-two/attachments/notes.txt", "Memory/preferences.md", "Index/search.sqlite", "Agent/config.json", "Skills/example/SKILL.md"];
foreach (string file in canonicalFiles)
{
    string path = Path.Combine(oldRoot, file);
    Directory.CreateDirectory(Path.GetDirectoryName(path)!);
    await File.WriteAllTextAsync(path, file.StartsWith("Backups") ? catalog : "{\"type\":\"message\",\"content\":\"你好\"}\n");
}
foreach (var item in new[] { (Id: "one", Folder: managed), (Id: "two", Folder: external) })
{
    string directory = Path.Combine(oldRoot, "Projects", item.Id);
    Directory.CreateDirectory(directory);
    await File.WriteAllTextAsync(Path.Combine(directory, "project.json"), System.Text.Json.JsonSerializer.Serialize(new
    { Version = 1, item.Id, FolderPath = item.Folder, Source = "../../catalog.json" }));
}
await File.WriteAllTextAsync(pointer, "{\"version\":1,\"dataRoot\":\"old\"}");
string originalPointer = await File.ReadAllTextAsync(pointer);
var target = Path.Combine(root, "new");
Directory.CreateDirectory(target); // The folder picker may return an existing empty directory.
bool initialized = false;
var result = await StorageMigrationService.MoveAsync(desktop, models, target, pointer, initializeTarget: async (destination, token) =>
{
    Check(destination == target, "initializer receives new data root");
    Check(await File.ReadAllTextAsync(pointer, token) == originalPointer, "initializer runs before pointer activation");
    Check(JsonNode.Parse(await File.ReadAllTextAsync(Path.Combine(destination, "catalog.json"), token))!["Projects"]![0]!["FolderPath"]!.GetValue<string>() == Path.Combine(target, "Desktop", "Projects", "one"), "initializer receives relocated metadata");
    Directory.CreateDirectory(Path.Combine(destination, "Projects", "two", "Memory"));
    initialized = true;
});
Check(initialized, "target initializer awaited");
Check(result.VerifiedFiles == 21, "legacy and canonical files verified");
Check(File.ReadAllText(Path.Combine(target, "Models", "connections.json")) == File.ReadAllText(Path.Combine(models, "connections.json")), "credentials copied intact");
Check(Directory.Exists(Path.Combine(target, "Desktop", "Projects", "one", "empty")), "empty folders preserved");
var projects = JsonNode.Parse(File.ReadAllText(Path.Combine(target, "Desktop", "projects.json")))!.AsArray();
Check(projects[0]!["FolderPath"]!.GetValue<string>() == Path.Combine(target, "Desktop", "Projects", "one"), "managed path remapped");
Check(projects[1]!["FolderPath"]!.GetValue<string>() == external, "external path retained");
var migratedCatalog = JsonNode.Parse(File.ReadAllText(Path.Combine(target, "catalog.json")))!;
Check(migratedCatalog["Projects"]![0]!["FolderPath"]!.GetValue<string>() == Path.Combine(target, "Desktop", "Projects", "one"), "canonical managed path remapped");
Check(migratedCatalog["Projects"]![1]!["FolderPath"]!.GetValue<string>() == external, "canonical external path retained");
Check(File.ReadAllText(Path.Combine(target, "settings.json")) == settings, "user settings and store identity copied intact");
Check(JsonNode.Parse(File.ReadAllText(Path.Combine(target, "Projects", "one", "project.json")))!["FolderPath"]!.GetValue<string>() == Path.Combine(target, "Desktop", "Projects", "one"), "derived project path remapped");
Check(JsonNode.Parse(File.ReadAllText(Path.Combine(target, "Projects", "two", "project.json")))!["FolderPath"]!.GetValue<string>() == external, "derived external project path retained");
Check(JsonNode.Parse(File.ReadAllText(Path.Combine(target, ".catalog-transaction.json")))!["catalog"]!["Projects"]![0]!["FolderPath"]!.GetValue<string>() == Path.Combine(target, "Desktop", "Projects", "one"), "pending catalog paths remapped");
Check(File.ReadAllText(Path.Combine(oldRoot, "catalog.json")) == catalog, "source catalog unchanged");
foreach (string file in canonicalFiles)
    Check(File.ReadAllText(Path.Combine(target, file)) == File.ReadAllText(Path.Combine(oldRoot, file)), "canonical history and backups copied intact: " + file);
Check(File.ReadAllText(Path.Combine(external, "untouched.txt")) == "keep", "external files untouched");
Check(File.ReadAllText(Path.Combine(target, "storage-pointer.previous.json")) == originalPointer, "old pointer backed up");
Check(JsonNode.Parse(File.ReadAllText(pointer))!["dataRoot"]!.GetValue<string>() == target, "pointer switched last");
string committedPointer = File.ReadAllText(pointer);
await Reject(() => StorageMigrationService.MoveAsync(desktop, models, target, pointer), "nonempty target");
await Reject(() => StorageMigrationService.MoveAsync(desktop, models, Path.Combine(desktop, "nested"), pointer), "nested target");
await Reject(() => StorageMigrationService.MoveAsync(desktop, models, Path.Combine(oldRoot, "Chats", "nested"), pointer), "canonical nested target");
await Reject(() => StorageMigrationService.MoveAsync(desktop, models, root, pointer), "ancestor target");
await Reject(() => StorageMigrationService.MoveAsync(desktop, models, "relative", pointer), "relative target");
using var cancel = new CancellationTokenSource(); cancel.Cancel();
await Reject(() => StorageMigrationService.MoveAsync(desktop, models, Path.Combine(root, "cancelled"), pointer, cancellationToken: cancel.Token), "cancelled migration");
await Reject(() => StorageMigrationService.MoveAsync(desktop, models, Path.Combine(root, "changed"), pointer,
    new ImmediateProgress(message => { if (message.StartsWith("正在核对")) File.AppendAllText(Path.Combine(desktop, "chats.json"), " "); })), "concurrent modification");
Check(File.ReadAllText(pointer) == committedPointer, "failure does not change pointer");
string[] invalidSettings = ["{", "null", "[]", "{\"Storage\":null}", "{\"Storage\":{\"LayoutVersion\":2}}", "{\"Storage\":{\"LayoutVersion\":\"1\"}}"];
for (int index = 0; index < invalidSettings.Length; index++)
{
    string rejectedTarget = Path.Combine(root, "invalid-layout-" + index);
    await File.WriteAllTextAsync(Path.Combine(oldRoot, "settings.json"), invalidSettings[index]);
    await Reject(() => StorageMigrationService.MoveAsync(desktop, models, rejectedTarget, pointer), "invalid/future layout");
    Check(!Directory.Exists(rejectedTarget), "invalid source settings rejected before copying");
    Check(File.ReadAllText(pointer) == committedPointer, "invalid settings preserve pointer");
}
await File.WriteAllTextAsync(Path.Combine(oldRoot, "settings.json"), settings);
string tamperedTarget = Path.Combine(root, "tampered-target");
await Reject(() => StorageMigrationService.MoveAsync(desktop, models, tamperedTarget, pointer,
    new ImmediateProgress(message => { if (message.StartsWith("正在核对")) File.AppendAllText(Path.Combine(tamperedTarget, "settings.json"), "invalid"); })), "invalid copied settings");
Check(File.ReadAllText(pointer) == committedPointer, "destination validated before pointer activation");
string failedInitializationTarget = Path.Combine(root, "failed-initialization");
await Reject(() => StorageMigrationService.MoveAsync(desktop, models, failedInitializationTarget, pointer,
    initializeTarget: (destination, token) => throw new InvalidOperationException("test initialization failure")), "target initialization fails");
Check(File.ReadAllText(pointer) == committedPointer, "initialization failure preserves pointer");
string changedSettingsTarget = Path.Combine(root, "initializer-invalid-settings");
await Reject(() => StorageMigrationService.MoveAsync(desktop, models, changedSettingsTarget, pointer,
    initializeTarget: (destination, token) => File.WriteAllTextAsync(Path.Combine(destination, "settings.json"), "{\"Storage\":{\"LayoutVersion\":99}}", token)), "initializer produces invalid layout");
Check(File.ReadAllText(pointer) == committedPointer, "settings validated after initialization");
string customModels = Path.Combine(root, "custom-model-home"), customTarget = Path.Combine(root, "custom-target");
Directory.CreateDirectory(Path.Combine(customModels, "Conversations", "Chats", "custom-chat"));
await File.WriteAllTextAsync(Path.Combine(customModels, "connections.json"), "{}");
await File.WriteAllTextAsync(Path.Combine(customModels, "Conversations", "catalog.json"), "{\"Version\":1,\"Projects\":[],\"Chats\":[]}");
await File.WriteAllTextAsync(Path.Combine(customModels, "Conversations", "Chats", "custom-chat", "events.jsonl"), "{\"content\":\"custom\"}\n");
await StorageMigrationService.MoveAsync(desktop, customModels, customTarget, pointer);
Check(File.Exists(Path.Combine(customTarget, "Chats", "custom-chat", "events.jsonl")), "custom conversation root relocated");
Check(!Directory.Exists(Path.Combine(customTarget, "Models", "Conversations")), "custom root not copied twice");
Console.WriteLine("PASS: legacy/canonical migration, history/backup hashes, managed/external metadata, custom roots, empty directories, target safety, cancellation and concurrent modification.");
// This directory contains only generated fixtures; leave it available for inspection.
Console.WriteLine("Fixture: " + root);

sealed class ImmediateProgress(Action<string> report) : IProgress<string> { public void Report(string value) => report(value); }
