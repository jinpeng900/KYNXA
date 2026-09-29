using System.Text.Json.Nodes;
using KYNXA_Desktop.Services;

static void Check(bool condition, string name) { if (!condition) throw new Exception(name); }
static async Task Reject(Func<Task> action, string name)
{
    try { await action(); } catch (Exception e) when (e is IOException or InvalidOperationException or OperationCanceledException) { return; }
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
await File.WriteAllTextAsync(pointer, "{\"version\":1,\"dataRoot\":\"old\"}");
string originalPointer = await File.ReadAllTextAsync(pointer);
var target = Path.Combine(root, "new");
Directory.CreateDirectory(target); // The folder picker may return an existing empty directory.
var result = await StorageMigrationService.MoveAsync(desktop, models, target, pointer);
Check(result.VerifiedFiles == 4, "all files verified");
Check(File.ReadAllText(Path.Combine(target, "Models", "connections.json")) == File.ReadAllText(Path.Combine(models, "connections.json")), "credentials copied intact");
Check(Directory.Exists(Path.Combine(target, "Desktop", "Projects", "one", "empty")), "empty folders preserved");
var projects = JsonNode.Parse(File.ReadAllText(Path.Combine(target, "Desktop", "projects.json")))!.AsArray();
Check(projects[0]!["FolderPath"]!.GetValue<string>() == Path.Combine(target, "Desktop", "Projects", "one"), "managed path remapped");
Check(projects[1]!["FolderPath"]!.GetValue<string>() == external, "external path retained");
Check(File.ReadAllText(Path.Combine(external, "untouched.txt")) == "keep", "external files untouched");
Check(File.ReadAllText(Path.Combine(target, "storage-pointer.previous.json")) == originalPointer, "old pointer backed up");
Check(JsonNode.Parse(File.ReadAllText(pointer))!["dataRoot"]!.GetValue<string>() == target, "pointer switched last");
string committedPointer = File.ReadAllText(pointer);
await Reject(() => StorageMigrationService.MoveAsync(desktop, models, target, pointer), "nonempty target");
await Reject(() => StorageMigrationService.MoveAsync(desktop, models, Path.Combine(desktop, "nested"), pointer), "nested target");
await Reject(() => StorageMigrationService.MoveAsync(desktop, models, root, pointer), "ancestor target");
await Reject(() => StorageMigrationService.MoveAsync(desktop, models, "relative", pointer), "relative target");
using var cancel = new CancellationTokenSource(); cancel.Cancel();
await Reject(() => StorageMigrationService.MoveAsync(desktop, models, Path.Combine(root, "cancelled"), pointer, cancellationToken: cancel.Token), "cancelled migration");
await Reject(() => StorageMigrationService.MoveAsync(desktop, models, Path.Combine(root, "changed"), pointer,
    new ImmediateProgress(message => { if (message.StartsWith("正在核对")) File.AppendAllText(Path.Combine(desktop, "chats.json"), " "); })), "concurrent modification");
Check(File.ReadAllText(pointer) == committedPointer, "failure does not change pointer");
Console.WriteLine("PASS: verified migration, empty directories, managed/external paths, backup, nonempty/nested targets, cancellation and concurrent modification.");
// This directory contains only generated fixtures; leave it available for inspection.
Console.WriteLine("Fixture: " + root);

sealed class ImmediateProgress(Action<string> report) : IProgress<string> { public void Report(string value) => report(value); }
