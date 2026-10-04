using KYNXA_Desktop.Services;

int checks = 0;
void Check(bool condition, string name)
{
    if (!condition) throw new InvalidOperationException(name);
    checks++;
}
string[] isolated = ["-y", "chrome-devtools-mcp@1.10.1", "--headless", "--isolated", "--no-usage-statistics", "--no-performance-crux"];
var chrome = BrowserConnectionSettings.Read("npx", isolated)!;
Check(chrome.Engine == "chrome-devtools" && chrome.Mode == BrowserConnectionMode.Independent && !chrome.ShowWindow, "isolated mode detection");
var existing = (chrome with { Mode = BrowserConnectionMode.Existing }).Apply(isolated);
Check(existing.Contains("--autoConnect") && !existing.Contains("--headless") && !existing.Contains("--isolated"), "existing browser connection has no conflicting launch flags");
Check(existing.Contains("--no-usage-statistics") && existing.Contains("chrome-devtools-mcp@1.10.1"), "pinned version and unrelated arguments retained");
Check(BrowserConnectionSettings.Read("npx", existing)!.Mode == BrowserConnectionMode.Existing, "saved existing browser reload");
var remote = (chrome with { Mode = BrowserConnectionMode.Remote, Endpoint = "wss://fixture.invalid/browser" }).Apply(isolated);
Check(remote.Contains("--wsEndpoint") && remote.Contains("wss://fixture.invalid/browser") && !remote.Contains("--isolated"), "remote WebSocket configuration");
Check(BrowserConnectionSettings.Read("npx", remote)!.Endpoint == "wss://fixture.invalid/browser", "remote endpoint reload");
var http = (chrome with { Mode = BrowserConnectionMode.Remote, Endpoint = "http://127.0.0.1:9222" }).Apply(existing);
Check(http.Contains("--browserUrl") && !http.Contains("--autoConnect"), "CDP HTTP replaces auto connection");
var visible = (chrome with { ShowWindow = true }).Apply(isolated);
Check(!visible.Contains("--headless") && visible.Contains("--isolated"), "visible independent browser");
string[] persistent = ["@playwright/mcp@0.0.83", "--headless=false", "--user-data-dir", "C:\\Synthetic\\Profile", "--profile-dir-name=Profile 1"];
var playwright = BrowserConnectionSettings.Read("npx", persistent)!;
var retained = (playwright with { ShowWindow = false }).Apply(persistent);
Check(retained.Contains("C:\\Synthetic\\Profile") && retained.Contains("--profile-dir-name=Profile 1") && !retained.Contains("--isolated"), "persistent profile remains intact when changing visibility");
var extension = (playwright with { Mode = BrowserConnectionMode.Existing }).Apply(persistent);
Check(extension.Contains("--extension") && !extension.Contains("--user-data-dir") && !extension.Contains("--headless"), "existing Playwright session uses extension instead of competing profile");
var cdp = (playwright with { Mode = BrowserConnectionMode.Remote, Endpoint = "wss://fixture.invalid/cdp" }).Apply(persistent);
Check(cdp.Contains("--cdp-endpoint") && !cdp.Contains("--user-data-dir"), "Playwright remote CDP");
string[] nativeEndpoint = ["@playwright/mcp@0.0.83", "--endpoint", "wss://fixture.invalid/playwright"];
var protocolEndpoint = BrowserConnectionSettings.Read("npx", nativeEndpoint)!;
var preservedEndpoint = (protocolEndpoint with { Endpoint = "wss://other.invalid/playwright" }).Apply(nativeEndpoint);
Check(preservedEndpoint.Contains("--endpoint") && !preservedEndpoint.Contains("--cdp-endpoint"), "existing Playwright endpoint protocol is preserved");
var environment = BrowserConnectionSettings.Read("npx", ["@playwright/mcp@0.0.83"], ["PLAYWRIGHT_MCP_CDP_ENDPOINT"])!;
Check(environment.Mode == BrowserConnectionMode.Custom && environment.HasEnvironmentConfiguration, "environment-driven connection is marked custom");
var config = BrowserConnectionSettings.Read("npx", ["@playwright/mcp@0.0.83", "--config=fixture.json"]);
Check(config!.Mode == BrowserConnectionMode.Custom, "configuration file mode is retained");
Check(config.Apply(["@playwright/mcp@0.0.83", "--config=fixture.json"]).SequenceEqual(["@playwright/mcp@0.0.83", "--config=fixture.json"]), "unmodified custom settings remain byte-equivalent arguments");
Check(BrowserConnectionSettings.Read("fixture-server", ["unrelated"]) is null, "unrelated MCP has no browser fields");
Check(BrowserConnectionSettings.Read("npx", ["chrome-devtools-mcp", "--headless", "false"])!.ShowWindow,
    "a separated false boolean keeps an independent browser visible");
Check(BrowserConnectionSettings.Read("npx", ["chrome-devtools-mcp", "--headless", "--headless=false"])!.ShowWindow,
    "the last explicit headless flag determines visibility");
Check(BrowserConnectionSettings.Read("npx", ["chrome-devtools-mcp", "--autoConnect", "false"])!.Mode == BrowserConnectionMode.Independent,
    "a separated false automatic connection does not select an existing browser");
Check(BrowserConnectionSettings.Read("npx", ["@playwright/mcp", "--extension", "false"])!.Mode == BrowserConnectionMode.Independent,
    "a separated false extension flag does not select an existing browser");
Check(BrowserConnectionSettings.Read("npx", ["chrome-devtools-mcp", null!]) is null,
    "a temporarily invalid JSON argument member does not crash the editor projection");
string[] directArgs = ["--user-data-dir", "C:\\Synthetic\\Profile", "--profile-dir-name", "Profile 2", "--headless"];
var direct = BrowserConnectionSettings.Read("C:\\Synthetic\\chrome-devtools-mcp.exe", directArgs)!;
Check((direct with { ShowWindow = true }).Apply(directArgs).Contains("Profile 2"),
    "direct-executable browser configurations preserve their selected profile when visibility changes");
foreach (string invalid in new[] { "file:///C:/Synthetic", "javascript:alert(1)", "https://user:pass@fixture.invalid", "wss://fixture.invalid/#fragment", "not an endpoint" })
{
    bool rejected = false;
    try { (chrome with { Mode = BrowserConnectionMode.Remote, Endpoint = invalid }).Apply(isolated); }
    catch (ArgumentException) { rejected = true; }
    Check(rejected, "invalid endpoint rejected");
}
var env = McpConfigurationInput.ReadReferences("{\"BROWSER_ENDPOINT\":\"FIXTURE_ENDPOINT\"}", headers: false);
Check(env["BROWSER_ENDPOINT"] == "FIXTURE_ENDPOINT", "MCP environment mappings retain reference names");
Check(McpConfigurationInput.ReadReferences("{\"X-Api-Key\":\"FIXTURE_TOKEN\"}", headers: true)["X-Api-Key"] == "FIXTURE_TOKEN",
    "HTTP header references use header names and environment-variable references");
foreach (string invalid in new[] { "null", "[]", "{\"invalid-name\":\"FIXTURE_TOKEN\"}", "{\"TOKEN\":null}", "{\"TOKEN\":\"literal fixture value\"}" })
{
    bool rejected = false;
    try { McpConfigurationInput.ReadReferences(invalid, headers: false); }
    catch (System.Text.Json.JsonException) { rejected = true; }
    Check(rejected, "malformed environment references are rejected before configuration is sent");
}
Check(McpConfigurationInput.ReadAuthentication(" ") is null, "an empty authentication editor has no authentication override");
Check(McpConfigurationInput.ReadAuthentication("{\"type\":\"bearer-env\",\"tokenEnv\":\"FIXTURE_TOKEN\"}")?.TokenEnv == "FIXTURE_TOKEN",
    "bearer authentication retains only the environment reference");
Check(McpConfigurationInput.ReadAuthentication("{\"type\":\"oauth-client-credentials\",\"clientId\":\"fixture-client\",\"clientSecretEnv\":\"FIXTURE_SECRET\",\"issuer\":\"https://issuer.test.invalid\"}")?.Type == "oauth-client-credentials",
    "OAuth client configuration preserves its explicit provider and environment reference");
foreach (string invalid in new[] { "null", "{\"type\":\"unsupported\"}", "{\"type\":\"bearer-env\",\"tokenEnv\":\"literal fixture token\"}",
    "{\"type\":\"oauth-client-credentials\",\"clientId\":\"fixture-client\",\"clientSecretEnv\":\"FIXTURE_SECRET\",\"issuer\":\"http://issuer.test.invalid\"}" })
{
    bool rejected = false;
    try { McpConfigurationInput.ReadAuthentication(invalid); }
    catch (System.Text.Json.JsonException) { rejected = true; }
    Check(rejected, "unsupported authentication inputs are rejected without reading credentials");
}
Console.WriteLine($"PASS: {checks} browser and MCP editor configuration checks.");
