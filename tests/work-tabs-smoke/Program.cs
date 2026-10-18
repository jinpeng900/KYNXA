using KYNXA_Desktop.Services;
using KYNXA.Contracts;

int checks = 0;
void Check(bool condition, string label)
{
    if (!condition) throw new InvalidOperationException(label);
    Console.WriteLine("PASS " + label);
    checks++;
}

var state = new ConversationWorkTabState();
var chatA = Guid.NewGuid();
var chatB = Guid.NewGuid();
var shot = new ConversationWorkTab("screenshot/message-a/call-a", "screenshot", "截图 1");
var terminal = new ConversationWorkTab("terminal/message-a/call-b", "terminal", "PowerShell · 执行中");
var secondShot = new ConversationWorkTab("screenshot/message-a/call-c", "screenshot", "截图 2");

var terminals = new ConversationWorkTabState();
var terminalOne = new ConversationWorkTab($"terminal/{chatA:D}/{Guid.NewGuid():N}", "terminal", "终端 1", chatA.ToString("D"));
var terminalTwo = terminalOne with { Key = $"terminal/{chatA:D}/{Guid.NewGuid():N}", Title = "终端 2" };
terminals.ShowConversation(chatA, [terminalOne, terminalTwo]);
Check(terminals.Select(terminalTwo.Key) && terminals.OpenItems.Count == 2,
    "Independent terminal tabs in the same conversation remain selectable.");
Check(terminals.Close(terminalOne.Key) && terminals.SelectedTab?.Key == terminalTwo.Key && terminals.OpenItems.Count == 1,
    "Closing one terminal preserves the selected sibling terminal.");
terminals.ShowConversation(chatB, []);
terminals.ShowConversation(chatA, [terminalOne, terminalTwo]);
Check(terminals.SelectedTab?.Key == terminalTwo.Key && terminals.OpenItems.Count == 1,
    "Returning to the conversation retains its surviving terminal and closed-tab choice.");
var sharedTerminal = ConversationWorkspaceSources.Terminal(chatA, "existing-session", "项目终端");
var renamedSharedTerminal = ConversationWorkspaceSources.Terminal(chatA, "existing-session", "已执行查询");
var sharedTerminals = new ConversationWorkTabState();
sharedTerminals.ShowConversation(chatA, [sharedTerminal]);
sharedTerminals.ShowConversation(chatA, [renamedSharedTerminal]);
Check(sharedTerminals.Items.Count == 1 && sharedTerminals.SelectedTab?.Key == sharedTerminal.Key &&
    sharedTerminals.SelectedTab.Version == "existing-session" && sharedTerminals.SelectedTab.Title == "已执行查询",
    "The same gateway PTY retains one logical terminal tab across duplicate attachments and title updates.");

Check(!state.ShowConversation(null, []) && !state.HasItems && !state.HasOpenTabs && state.SelectedTab is null,
    "No active chat exposes neither tabs nor another chat's selection.");
Check(state.ShowConversation(chatA, [shot]) && state.SelectedTab?.Key == shot.Key,
    "The first available resource opens and becomes selected.");
Check(!state.ShowConversation(chatA, [shot]), "Repeated snapshots do not report newly discovered resources.");
Check(state.ShowConversation(chatA, [shot, terminal]) && state.OpenItems.Count == 2 && state.SelectedTab?.Key == shot.Key,
    "A new command opens as a background tab and preserves the screenshot being read.");
Check(state.Select(terminal.Key) && state.SelectedTab?.Key == terminal.Key,
    "The user can select the exact command resource.");
Check(state.ShowConversation(chatA, [shot, terminal, secondShot]) && state.SelectedTab?.Key == terminal.Key,
    "A later screenshot never steals the user's selected terminal.");
Check(!state.Select("missing") && state.SelectedTab?.Key == terminal.Key,
    "An unknown tab cannot change the selected resource.");

var renamedTerminal = terminal with { Title = "PowerShell · 已完成 · 0" };
Check(!state.ShowConversation(chatA, [shot, renamedTerminal, secondShot]) && state.SelectedTab?.Title == renamedTerminal.Title,
    "A status/title update refreshes presentation while retaining logical resource identity.");
Check(state.Close(terminal.Key) && state.SelectedTab?.Key != terminal.Key && state.Items.Count == 3 && state.OpenItems.Count == 2,
    "Closing a selected tab hides it without removing its resource catalog entry.");
Check(!state.ShowConversation(chatA, [shot, renamedTerminal with { Title = "Archive loaded" }, secondShot]) &&
    state.OpenItems.All(item => item.Key != terminal.Key),
    "Refreshing a closed resource's title or archive does not reopen it.");
Check(!state.Select(terminal.Key), "A closed tab requires an explicit reopen rather than incidental selection.");
Check(state.Reopen(terminal.Key) && state.SelectedTab?.Key == terminal.Key && state.OpenItems.Count == 3,
    "Explicit reopen restores and selects the retained resource.");
Check(state.OpenItems.Select(item => item.Key).SequenceEqual([shot.Key, terminal.Key, secondShot.Key]),
    "Reopening preserves source order rather than moving resources arbitrarily.");
Check(state.CloseSelected() && state.OpenItems.Count == 2,
    "Close selected hides exactly the current resource.");
state.Select(secondShot.Key);
state.Close(shot.Key);
Check(state.CloseSelected() && !state.HasOpenTabs && state.SelectedTab is null && state.HasItems,
    "Closing all tabs leaves a reopenable catalog and no visible content.");
state.ShowConversation(chatA, [shot, renamedTerminal, secondShot]);
Check(!state.HasOpenTabs && state.SelectedTab is null,
    "An ordinary refresh cannot undo the user's close-all choice.");
var fresh = new ConversationWorkTab("terminal/message-next/call-new", "terminal", "cmd");
Check(state.ShowConversation(chatA, [shot, renamedTerminal, secondShot, fresh]) &&
    state.OpenItems.Count == 1 && state.SelectedTab?.Key == fresh.Key,
    "A genuinely new resource becomes selected when no tab is open.");
state.Reopen(secondShot.Key);
state.ShowConversation(chatB, [terminal]);
Check(state.SelectedTab?.Key == terminal.Key && state.OpenItems.Count == 1,
    "A different chat starts with its own resource selection and closure state.");
state.ShowConversation(chatA, [shot, renamedTerminal, secondShot, fresh]);
Check(state.SelectedTab?.Key == secondShot.Key && state.OpenItems.Count == 2 &&
    state.OpenItems.All(item => item.Key != shot.Key && item.Key != terminal.Key),
    "Returning to a chat restores its selected history item and closed tabs.");
state.ShowConversation(chatA, [fresh, secondShot, renamedTerminal, shot]);
Check(state.SelectedTab?.Key == secondShot.Key && state.OpenItems.Select(item => item.Key).SequenceEqual([fresh.Key, secondShot.Key]),
    "Snapshot reordering changes tab order without changing the user's selection.");
state.ShowConversation(chatA, [fresh]);
Check(state.SelectedTab?.Key == fresh.Key && state.Items.Count == 1,
    "Removing the selected source falls back only to a resource still present in the same chat.");
state.ShowConversation(null, []);
Check(state.ConversationId is null && state.Items.Count == 0 && state.SelectedTab is null,
    "Leaving work mode cannot retain a visible tab from the old conversation.");

var filtered = new ConversationWorkTabState();
filtered.ShowConversation(chatA, [shot, shot with { Title = "duplicate" },
    new(" ", "screenshot", "empty key"), new("unknown", "plugin-not-yet-supported", "unsupported"), terminal]);
Check(filtered.Items.Count == 2 && filtered.Items.Select(item => item.Key).SequenceEqual([shot.Key, terminal.Key]),
    "Duplicate logical keys, blank keys and unsupported kinds cannot create ambiguous tabs.");
Check(!filtered.Close("absent") && !filtered.Reopen("absent"),
    "Unknown close/reopen requests leave valid tab state unchanged.");
filtered.ShowConversation(Guid.Empty, [shot]);
Check(filtered.ConversationId is null && !filtered.HasItems,
    "An empty conversation ID cannot own resource tabs.");

var rehydrating = new ConversationWorkTabState();
rehydrating.ShowConversation(chatA, [shot, secondShot]);
rehydrating.Close(shot.Key);
rehydrating.ShowConversation(chatA, []);
Check(!rehydrating.HasItems && rehydrating.SelectedTab is null,
    "A temporary empty source projection does not expose a stale resource.");
Check(!rehydrating.ShowConversation(chatA, [shot]) && !rehydrating.HasOpenTabs,
    "A closed resource returning in a partial snapshot is not discovered as a newly open tab.");
rehydrating.ShowConversation(chatA, [shot, secondShot]);
Check(rehydrating.SelectedTab?.Key == secondShot.Key && rehydrating.OpenItems.Count == 1,
    "History rehydration retains recent close choices and restores the only open resource.");
rehydrating.ShowConversation(chatA, []);
rehydrating.ShowConversation(chatA, [shot, secondShot]);
Check(rehydrating.SelectedTab?.Key == secondShot.Key && rehydrating.OpenItems.Count == 1,
    "A complete empty refresh preserves the logical selected key without reopening its closed neighbor.");

var absentClosures = new ConversationWorkTabState();
var closedHistory = Enumerable.Range(0, ConversationWorkTabState.MaximumAbsentClosedTabs + 1)
    .Select(index => new ConversationWorkTab("screenshot/history/" + index, "screenshot", "Screenshot " + index)).ToArray();
absentClosures.ShowConversation(chatA, closedHistory);
foreach (var item in closedHistory) { absentClosures.Reopen(item.Key); absentClosures.Close(item.Key); }
absentClosures.ShowConversation(chatA, []);
absentClosures.ShowConversation(chatA, [closedHistory[0], closedHistory[^1]]);
Check(absentClosures.OpenItems.Count == 1 && absentClosures.SelectedTab?.Key == closedHistory[0].Key,
    "Absent-resource closure metadata is bounded by evicting the oldest display choice.");

filtered.ShowConversation(chatA, [shot, terminal, new("unsafe\nkey", "screenshot", "control character"),
    new(new string('k', 513), "terminal", "oversize key"), new("long-title", "terminal", new string('t', 257))]);
Check(filtered.Items.Count == 2, "Control-character identities and oversized descriptors cannot enter the tab catalog.");

var hidden = new ConversationWorkTabState();
hidden.ShowConversation(chatA, [shot], autoOpenNew: false);
Check(hidden.HasItems && !hidden.HasOpenTabs && hidden.SelectedTab is null,
    "A globally hidden sidebar can discover a resource without opening or selecting it.");
hidden.Reopen(shot.Key);
hidden.ShowConversation(chatA, [shot, terminal], autoOpenNew: false);
Check(hidden.SelectedTab?.Key == shot.Key && hidden.OpenItems.Count == 1 && hidden.Items.Count == 2,
    "Discovery while hidden preserves existing explicit selection and leaves newly discovered items closed.");

var bounded = new ConversationWorkTabState();
var oldest = Guid.NewGuid();
bounded.ShowConversation(oldest, [shot, terminal]);
bounded.Select(terminal.Key); bounded.Close(shot.Key);
for (int i = 0; i < 15; i++) bounded.ShowConversation(Guid.NewGuid(), [shot, terminal]);
bounded.ShowConversation(oldest, [shot, terminal]);
Check(bounded.SelectedTab?.Key == terminal.Key && bounded.OpenItems.Count == 1,
    "The sixteenth retained conversation preserves its selection and closed keys.");
for (int i = 0; i < 16; i++) bounded.ShowConversation(Guid.NewGuid(), [shot, terminal]);
bounded.ShowConversation(oldest, [shot, terminal]);
Check(bounded.SelectedTab?.Key == shot.Key && bounded.OpenItems.Count == 2,
    "LRU eviction bounds old display state and rehydrates evicted chats from current resources.");

var screenshotReference = new ToolResultReference(Guid.NewGuid(), 8192, new string('a', 64));
var screenshotNames = new[] { "computer.screenshot", "browser.screenshot", "mcp.windows-screenshot.Screenshot",
    "mcp.custom-browser.browser_take_screenshot", "mcp.browser-debug.take_screenshot" };
var screenshotTools = screenshotNames.Select((name, index) => new ToolActivity("capture-" + index, name, null,
    "completed", "Owned screenshot fixture", ResultRef: screenshotReference with { Id = Guid.NewGuid() })).ToArray();
var discoveredScreenshots = ConversationScreenshotSources.Collect(chatA,
    [new(chatA, Guid.NewGuid(), "assistant", screenshotTools)]);
Check(discoveredScreenshots.Length == screenshotNames.Length,
    "Native browser and Windows MCP screenshot receipts reach the existing sidebar source projection.");
Check(!ConversationScreenshotSources.IsScreenshotTool("mcp.windows-screenshot.windows") &&
    !ConversationScreenshotSources.IsScreenshotTool("user.Screenshot"),
    "Window inventories and model labels are not screenshot capabilities.");
Check(ConversationScreenshotSources.Collect(chatA,
    [new(chatB, Guid.NewGuid(), "assistant", screenshotTools)]).Length == 0 &&
    ConversationScreenshotSources.Collect(chatA,
    [new(chatA, Guid.NewGuid(), "assistant", [screenshotTools[0] with { Status = "unknown" }])]).Length == 0,
    "Foreign conversations and unknown execution outcomes cannot add sidebar screenshots.");

var documentTabs = new ConversationWorkTabState();
var document = ConversationWorkspaceSources.File(@"D:\work\报告.md", version: "one");
var sameDocument = ConversationWorkspaceSources.File(@"d:/work/报告.md", version: "two");
Check(document.Key == sameDocument.Key, "The same Windows file keeps one logical tab across case, separator and version changes.");
var diagram = ConversationWorkspaceSources.File(@"D:\work\流程.mmd");
var browser = ConversationWorkspaceSources.FromReference(System.Text.Json.JsonSerializer.SerializeToElement(
    new { kind = "browser", path = "https://example.org/article", url = "https://example.org/article", title = "资料来源" }))!;
documentTabs.ShowConversation(chatA, [document, diagram, browser]);
Check(documentTabs.Items.Count == 3 && diagram.Kind == "diagram" && browser.Kind == "browser",
    "Documents, Mermaid and website tabs share the existing conversation strip.");
documentTabs.Select(browser.Key); documentTabs.ShowConversation(chatA, [sameDocument, diagram, browser]);
Check(documentTabs.SelectedTab?.Key == browser.Key && documentTabs.Items[0].Version == "two",
    "Updating an artifact refreshes its version without stealing the selected webpage.");
Check(documentTabs.Move(browser.Key, document.Key), "The user can reorder document and browser tabs.");
documentTabs.ShowConversation(chatA, [sameDocument, diagram, browser]);
Check(documentTabs.Items.Select(tab => tab.Key).SequenceEqual([browser.Key, document.Key, diagram.Key]),
    "User tab ordering survives later artifact snapshots.");
var fileReceipt = System.Text.Json.JsonSerializer.SerializeToElement(new { path = @"D:\work\成品.docx", sha256 = "actual-version", title = "成品" });
var write = new ToolActivity("write-artifact", "filesystem.write", null, "completed", "Saved");
Check(ConversationWorkspaceSources.FromTool(write, fileReceipt) is { Title: "成品", Version: "actual-version", Kind: "document" },
    "A successful real write receipt exposes its actual filename, model title and version.");
Check(ConversationWorkspaceSources.FromTool(write with { Status = "error" }, fileReceipt) is null &&
    ConversationWorkspaceSources.FromTool(write with { Status = "unknown" }, fileReceipt) is null,
    "Failed and unknown writes cannot masquerade as existing output files.");
Check(ConversationWorkspaceSources.FromTool(write, System.Text.Json.JsonSerializer.SerializeToElement(new { message = "report.md created" })) is null,
    "Prose that mentions a filename cannot invent a resource tab.");
Check(ConversationWorkspaceSources.FromTool(write with { Name = "document.read" }, fileReceipt) is null &&
    ConversationWorkspaceSources.FromTool(write with { Name = "filesystem.read" }, fileReceipt) is null,
    "Ordinary source reads do not create dozens of background tabs; artifact.open explicitly chooses what to display.");
var redirectedWebsite = System.Text.Json.JsonSerializer.SerializeToElement(new
    { kind = "browser", presentation = "workspace", url = "https://example.org/article", title = "Article" });
Check(ConversationWorkspaceSources.FromTool(write with { Name = "browser.open" }, redirectedWebsite) is { Kind: "browser", Title: "Article" }
    && ConversationWorkspaceSources.FromTool(write with { Name = "computer.launch" }, redirectedWebsite) is { Kind: "browser" },
    "Actual workspace browser receipts open sidebar tabs even when the model initially selected an application tool.");
var pendingWebsite = System.Text.Json.JsonSerializer.SerializeToElement(new
    { presentation = "workspace", url = "about:blank", path = "https://example.org/loading", navigationCommitted = false });
Check(ConversationWorkspaceSources.FromTool(write with { Name = "browser.open" }, pendingWebsite)
    is { Kind: "browser", Target: "https://example.org/loading" },
    "An already opened page still loading its first URL remains an activatable browser tab.");
Check(ConversationWorkspaceSources.FromTool(write with { Name = "computer.launch" },
    System.Text.Json.JsonSerializer.SerializeToElement(new { url = "https://example.org/article" })) is null,
    "An ordinary external browser launch cannot create a sidebar tab without a workspace presentation receipt.");
documentTabs.Close(document.Key); documentTabs.ShowConversation(chatA, [sameDocument, diagram, browser]);
Check(documentTabs.OpenItems.All(tab => tab.Key != document.Key) && documentTabs.Items.Any(tab => tab.Key == document.Key),
    "Closing a file tab preserves the real artifact and its closed choice after updates.");

Check(documentTabs.SelectHome() && documentTabs.SelectedTab is null && documentTabs.HasOpenTabs,
    "The workspace home preserves open file and browser tabs without selecting a hidden resource.");
documentTabs.ShowConversation(chatA, [sameDocument, diagram, browser]);
Check(documentTabs.SelectedTab is null && documentTabs.OpenItems.Count == 2,
    "Routine refreshes keep the explicit home view and preserve closed file choices.");
Check(documentTabs.Select(browser.Key) && documentTabs.SelectedTab?.Key == browser.Key,
    "Selecting a retained browser tab leaves the home view and resumes that resource.");

var automaticTabs = new ConversationWorkTabState();
var savedOutput = ConversationWorkspaceSources.FromTool(write, fileReceipt)!;
automaticTabs.ShowConversation(chatA, [savedOutput]);
Check(savedOutput.PresentationPriority == WorkspacePresentationPriority.CatalogOnly &&
    automaticTabs.Items.Count == 1 && !automaticTabs.HasOpenTabs,
    "Saving an ordinary output catalogs its real path without opening a preview.");
automaticTabs.ShowConversation(chatA, [savedOutput with { Version = "next-version" }]);
Check(automaticTabs.Items.Count == 1 && automaticTabs.Items[0].Version == "next-version" && !automaticTabs.HasOpenTabs,
    "Repeated intermediate writes update the one catalog entry without creating or opening tabs.");
automaticTabs.Reopen(savedOutput.Key);
var presentationOutputs = Enumerable.Range(0, 12).Select(index => ConversationWorkspaceSources.FromTool(
    write with { ToolCallId = "present-report-" + index, Name = "artifact.open" },
    System.Text.Json.JsonSerializer.SerializeToElement(new { path = $@"D:\work\output-{index}.md", title = "报告 " + index }))!).ToArray();
automaticTabs.ShowConversation(chatA, [savedOutput, .. presentationOutputs]);
Check(automaticTabs.OpenItems.Count == presentationOutputs.Length + 1 && automaticTabs.OpenItems.Count > 4 &&
    automaticTabs.SelectedTab?.Key == savedOutput.Key && automaticTabs.Items.Count == 13,
    "More than four useful reports can be presented together without displacing the user's explicitly opened file.");
var recentOutput = presentationOutputs[^1];
Check(automaticTabs.Select(recentOutput.Key), "A visible background output can become a protected user selection.");
var delivered = ConversationWorkspaceSources.PrioritizeDeliveredOutputs([savedOutput, .. presentationOutputs],
    "最终成果：[成品](<D:/work/成品.docx>)；[报告](output-0.md)。");
automaticTabs.ShowConversation(chatA, delivered);
Check(automaticTabs.OpenItems.Any(tab => tab.Key == presentationOutputs[0].Key) &&
    automaticTabs.SelectedTab?.Key == recentOutput.Key &&
    delivered.Single(tab => tab.Key == presentationOutputs[0].Key).PresentationPriority == WorkspacePresentationPriority.Final,
    "A delivered file outside the recent batch is promoted and shown without displacing the user's selected output.");
Check(automaticTabs.OpenItems.Select(tab => tab.Key).ToHashSet().SetEquals(delivered.Select(tab => tab.Key)),
    "Prioritizing final deliverables does not discard other relevant summaries or reports by count.");
automaticTabs.Close(presentationOutputs[0].Key);
automaticTabs.ShowConversation(chatA, delivered);
Check(automaticTabs.OpenItems.All(tab => tab.Key != presentationOutputs[0].Key),
    "A refreshed final-delivery link cannot undo an explicit close choice.");

var actualFinal = ConversationWorkspaceSources.FromTool(write with { ToolCallId = "final-write" },
    System.Text.Json.JsonSerializer.SerializeToElement(new { path = @"D:\work\deliveries\最终报告.md", sha256 = "final-version" }))!;
var finalProjection = ConversationWorkspaceSources.PrioritizeDeliveredOutputs([actualFinal, savedOutput],
    "结果：[最终报告](<D:/work/deliveries/最终报告.md>)；[不存在](missing.md)。");
var deliveryState = new ConversationWorkTabState();
deliveryState.ShowConversation(chatA, finalProjection);
Check(finalProjection.Length == 2 && deliveryState.OpenItems.Count == 1 &&
    deliveryState.SelectedTab?.Key == actualFinal.Key &&
    finalProjection.Single(tab => tab.Key == savedOutput.Key).PresentationPriority == WorkspacePresentationPriority.CatalogOnly,
    "Only a known delivered file is opened; links do not invent missing files or expose intermediate outputs.");
var ambiguous = ConversationWorkspaceSources.PrioritizeDeliveredOutputs([
    ConversationWorkspaceSources.File(@"D:\work\a\same.md") with { PresentationPriority = WorkspacePresentationPriority.CatalogOnly },
    ConversationWorkspaceSources.File(@"D:\work\b\same.md") with { PresentationPriority = WorkspacePresentationPriority.CatalogOnly }],
    "[报告](same.md)");
Check(ambiguous.All(tab => tab.PresentationPriority == WorkspacePresentationPriority.CatalogOnly),
    "An ambiguous relative delivery link cannot promote an arbitrary file with the same name.");

var automaticReader = new ConversationWorkTabState();
automaticReader.ShowConversation(chatB, [presentationOutputs[0]]);
automaticReader.ShowConversation(chatB, presentationOutputs);
Check(automaticReader.OpenItems.Count == presentationOutputs.Length &&
    automaticReader.SelectedTab?.Key == presentationOutputs[0].Key &&
    automaticReader.OpenItems.Any(tab => tab.Key == presentationOutputs[^1].Key),
    "Later batches preserve the active reader and every requested output without a fixed first-N cutoff.");
var requestedPages = presentationOutputs.Select(tab => tab with { PresentationPriority = WorkspacePresentationPriority.Requested }).ToArray();
var mixedPresentations = new ConversationWorkTabState();
mixedPresentations.ShowConversation(chatB, [requestedPages[0]]);
mixedPresentations.ShowConversation(chatB, [.. requestedPages, finalProjection[0]]);
Check(mixedPresentations.OpenItems.Count == requestedPages.Length + 1 &&
    mixedPresentations.OpenItems.Any(tab => tab.Key == actualFinal.Key) &&
    mixedPresentations.OpenItems.Any(tab => tab.Key == requestedPages[^1].Key) &&
    mixedPresentations.SelectedTab?.Key == requestedPages[0].Key,
    "Final delivery and all requested presentations coexist without stealing the active reader.");

Check(WorkspaceNavigationAddress.TryResolve("  https://example.org/article?q=physics  ", out var directAddress)
    && directAddress.AbsoluteUri == "https://example.org/article?q=physics"
    && WorkspaceNavigationAddress.TryResolve("example.org/article", out var bareAddress)
    && bareAddress.AbsoluteUri == "https://example.org/article",
    "Both explicit URLs and bare website addresses navigate directly.");
Check(WorkspaceNavigationAddress.TryResolve("量子 力学 & 实验", out var searchAddress)
    && searchAddress.Host == "www.google.com" && searchAddress.AbsolutePath == "/search"
    && searchAddress.Query == "?q=" + Uri.EscapeDataString("量子 力学 & 实验"),
    "Ordinary input becomes one escaped Google query shared by both address bars.");
Check(!WorkspaceNavigationAddress.TryResolve("  ", out _) && !WorkspaceNavigationAddress.TryResolve("file:///C:/test.txt", out _),
    "Empty input and non-web navigation do not open a browser page.");

Console.WriteLine($"PASS: {checks} work tab state, artifact and screenshot receipt checks.");
