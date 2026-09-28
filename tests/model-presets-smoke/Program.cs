using KYNXA_Desktop.Services;

static void Check(bool value, string message)
{
    if (!value) throw new Exception(message);
}

Check(ModelPresets.Recognize("  DEEPSEEK api ")?.Id == "deepseek", "Typed service name must ignore casing and whitespace.");
Check(ModelPresets.Recognize("我的工作连接") is null, "Custom names must not be reinterpreted.");
Check(ModelPresets.UniqueId("deepseek", ["deepseek", "deepseek-2", "deepseek-4"]) == "deepseek-3",
    "Adding the same service must not overwrite a saved route.");
Check(ModelPresets.UniqueId("kimi", ["deepseek"]) == "kimi", "Unoccupied IDs should stay readable.");
Check(ModelPresets.All.Select(p => p.Id).Distinct().Count() == ModelPresets.All.Count, "Preset IDs must be unique.");
Check(ModelPresets.All.First(p => p.Id == "anthropic").Protocol == "anthropic-messages", "Claude must use Messages.");
Check(ModelPresets.All.First(p => p.Id == "openai").Protocol == "openai-responses", "OpenAI must use Responses.");
foreach (var id in ModelPresets.All.SelectMany(p => p.Models))
    Check(ModelCatalog.Describe(id).Description.Length > 0, "Preset models need a description.");
Check(ModelCatalog.Describe("my-custom-model").Id == "my-custom-model", "Custom model IDs must remain exact.");
foreach (var preset in ModelPresets.All.Where(p => p.Id != "custom"))
{
    Check(Uri.TryCreate(preset.BaseUrl, UriKind.Absolute, out var uri), "Preset must have an absolute endpoint.");
    Check(uri!.IsLoopback || uri.Scheme == "https", "Cloud presets must use HTTPS.");
    if (!uri.IsLoopback) Check(preset.Models.Length > 0, "Cloud setup must work without querying /models first.");
}
Console.WriteLine("Model preset checks passed: name recognition, collision avoidance, endpoints and default IDs.");
Check(ModelPresets.All.Any(p => p.Id == "local-api"), "Direct local API must be a standalone preset.");
foreach (string host in new[] { "localhost", "127.0.0.2", "10.1.2.3", "172.16.0.1", "172.31.255.254",
    "192.168.1.25", "[::1]", "[fd12::1]", "[fc00::1]", "[::ffff:192.168.1.2]" })
    Check(ModelPresets.IsLocalEndpoint(new Uri($"http://{host}:8080/v1")), $"Local address rejected: {host}");
foreach (string host in new[] { "example.com", "192.168.1.2.example.com", "172.15.0.1", "172.32.0.1",
    "192.169.1.1", "8.8.8.8", "[2001:4860::1]", "[::ffff:8.8.8.8]" })
    Check(!ModelPresets.IsLocalEndpoint(new Uri($"http://{host}/v1")), $"Public address treated as local: {host}");
Console.WriteLine("Direct local API and LAN address checks passed.");
