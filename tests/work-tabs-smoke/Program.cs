using KYNXA_Desktop.Services;

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
foreach (var item in closedHistory) absentClosures.Close(item.Key);
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

Console.WriteLine($"PASS: {checks} work tab state checks.");
