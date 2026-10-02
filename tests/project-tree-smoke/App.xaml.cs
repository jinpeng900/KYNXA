using System.Collections.Specialized;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Automation.Provider;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using SmokePage = KYNXA_Desktop.Views.ShellPage;

namespace ProjectTreeSmoke;

public partial class App : Application
{
    private readonly string _result = Path.Combine(Path.GetTempPath(), "kynxa-project-tree-smoke.txt");
    private readonly List<string> _checks = [];
    private Window? _window;
    private bool _successful;

    public App()
    {
        InitializeComponent();
        UnhandledException += (_, error) => File.WriteAllText(_result, "FAIL: " + error.Exception);
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        File.WriteAllText(_result, "RUNNING: isolated native TreeView regression checks");
        var page = new SmokePage();
        _window = new Window { Title = "KYNXA isolated project tree smoke", Content = page };
        KYNXA_Desktop.App.Window = _window;
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(550, 700));
        _window.Closed += (_, _) =>
        {
            page.CloseView();
            if (_successful) File.WriteAllText(_result,
                string.Join(Environment.NewLine, _checks) + $"\nPASS: {_checks.Count} checks; native window closed successfully.");
        };
        _window.Activate();
        _ = RunAsync(page);
    }

    private async Task RunAsync(SmokePage page)
    {
        try
        {
            await Task.Delay(300);
            var alpha = new ProjectState { Name = "Alpha", Chats = [SavedChat("Alpha chat"), SavedChat("Alpha second")] };
            var beta = new ProjectState { Name = "Beta", Chats = [SavedChat("Beta chat")] };
            var pinned = new ProjectState { Name = "Pinned", IsPinned = true, Chats = [SavedChat("Pinned chat")] };
            var folderless = new ProjectState { Name = "Hidden workspace", IsFolderlessWorkspace = true };
            var archived = new ProjectState { Name = "Archived", IsArchived = true };
            var projects = new List<ProjectState> { alpha, beta, pinned, folderless, archived };
            var collapsed = new HashSet<Guid>();
            var expand = new HashSet<Guid>();
            int resets = 0, changes = 0;
            void Track(object? sender, NotifyCollectionChangedEventArgs args)
            {
                changes++;
                if (args.Action == NotifyCollectionChangedAction.Reset) resets++;
            }
            page.Entries.CollectionChanged += Track;
            void Update(Guid? chatId = null, Guid? selectedProjectId = null)
            {
                ProjectTreeReconciler.Update(page.Entries, projects, chatId, collapsed, expand, selectedProjectId);
                expand.Clear();
            }
            Update(alpha.Chats[0].Id, alpha.Id);
            foreach (var row in page.Entries) row.Children.CollectionChanged += Track;
            await Settle(page);
            var alphaRow = page.Entries.Single(row => row.Project.Id == alpha.Id);
            var alphaChat = alphaRow.Children[0];
            var alphaContainer = page.ProjectTree.ContainerFromItem(alphaRow);
            Check(page.Entries.Count == 3 && page.Entries[0].Project.Id == pinned.Id, "filters hidden/archived projects and puts pinned projects first");
            Check(alphaContainer is FrameworkElement && page.ProjectTree.ContainerFromItem(alphaChat) is null,
                "real TreeView initially realizes the collapsed project without its child chat containers");
            Check(!alphaRow.IsExpanded && alphaChat.IsActive && alphaRow.ActiveVisibility == Visibility.Visible,
                "opening a chat highlights its selected project without automatically expanding it");
            Check(page.ProjectTree.SelectionMode == TreeViewSelectionMode.None, "native TreeView selection is disabled");

            expand.Add(alpha.Id);
            Update(alpha.Chats[0].Id, alpha.Id);
            await Settle(page);
            Check(alphaRow.IsExpanded && page.ProjectTree.ContainerFromItem(alphaChat) is FrameworkElement,
                "explicit expansion realizes chat containers while retaining the selected project row");

            Update(beta.Chats[0].Id, beta.Id);
            await Settle(page);
            var betaRow = page.Entries.Single(row => row.Project.Id == beta.Id);
            Check(alphaRow.IsExpanded && !betaRow.IsExpanded && betaRow.IsActive && !alphaRow.IsActive,
                "switching chats preserves manual expansion and keeps the next selected project collapsed");
            Update(selectedProjectId: beta.Id);
            await Settle(page);
            Check(betaRow.IsActive && page.Entries.SelectMany(row => row.Children).All(row => !row.IsActive),
                "selecting a workspace highlights its root even when no chat is open");
            Update(alpha.Chats[0].Id, alpha.Id);
            await Settle(page);

            int unchangedCount = changes;
            Update(alpha.Chats[0].Id);
            await Settle(page);
            Check(changes == unchangedCount && ReferenceEquals(alphaContainer, page.ProjectTree.ContainerFromItem(alphaRow)),
                "unchanged updates preserve native container and make no collection changes");

            alpha.Name = "Renamed Alpha";
            alpha.Chats[0].Title = "Renamed chat";
            Update(alpha.Chats[0].Id);
            await Settle(page);
            Check(ReferenceEquals(alphaRow, page.Entries.Single(row => row.Project.Id == alpha.Id)) && ReferenceEquals(alphaChat, alphaRow.Children[0]),
                "renaming keeps project and chat row identity");
            Check(Descendants<TextBlock>(page).Any(text => text.Text == "Renamed Alpha") &&
                  Descendants<TextBlock>(page).Any(text => text.Text == "Renamed chat"), "one-way bindings render renamed project and chat titles");
            Check(Descendants<Border>((DependencyObject)page.ProjectTree.ContainerFromItem(alphaChat)).Any(border =>
                  border.Name == "ActiveBackground" && border.Visibility == Visibility.Visible), "active background visibility binding reaches the native visual tree");

            collapsed.Add(alpha.Id);
            Update(alpha.Chats[0].Id);
            await Settle(page);
            Check(!alphaRow.IsExpanded, "manual collapse remains intact while its chat is active");
            collapsed.Remove(alpha.Id);
            Update(alpha.Chats[0].Id, alpha.Id);
            await Settle(page);
            Check(!alphaRow.IsExpanded, "an active chat does not reopen a collapsed project after the override is cleared");
            expand.Add(alpha.Id);
            Update();
            await Settle(page);
            Check(alphaRow.IsExpanded && !alphaChat.IsActive, "explicit expansion and clearing active state work independently");

            beta.IsPinned = true;
            alpha.Chats[1].IsPinned = true;
            Update(alpha.Chats[0].Id);
            await Settle(page);
            Check(page.Entries[0].Project.Id == beta.Id && page.Entries[1].Project.Id == pinned.Id,
                "pinning preserves source order among pinned projects");
            Check(alphaRow.Children[0].Chat!.Id == alpha.Chats[1].Id && alphaRow.Children[1] == alphaChat,
                "chat pinning reorders existing rows");
            Check(page.Entries[0].Glyph == "\uE718", "pin glyph refreshes in place");

            var reloaded = new ProjectState { Id = alpha.Id, Name = "Reloaded Alpha", Chats = alpha.Chats.Select(chat =>
                new ProjectChatState { Id = chat.Id, Title = chat.Title, IsPinned = chat.IsPinned,
                    Messages = chat.Messages.ToList() }).ToList() };
            projects[0] = reloaded;
            Update(reloaded.Chats[0].Id);
            await Settle(page);
            Check(alphaRow.Project == reloaded && alphaChat.Chat == reloaded.Chats[0] && alphaChat.Project == reloaded,
                "catalog reload refreshes model references without replacing rows");

            var draft = new ProjectChatState { Title = "Empty draft" };
            reloaded.Chats.Add(draft);
            Update(draft.Id);
            await Settle(page);
            Check(alphaRow.Children.All(row => row.Chat != draft), "an active empty draft is excluded from project history");
            draft.Messages.Add(new ChatMessageState { Content = "First message" });
            Update(draft.Id);
            await Settle(page);
            Check(alphaRow.Children.Any(row => row.Chat == draft && row.IsActive), "sending the first message makes the chat appear and highlights it");
            reloaded.Chats.Remove(draft);
            reloaded.Chats[1].IsArchived = true;
            beta.IsArchived = true;
            Update(reloaded.Chats[0].Id);
            await Settle(page);
            Check(page.Entries.Count == 2 && alphaRow.Children.Count == 1 && alphaRow.Children[0] == alphaChat,
                "archiving and empty-draft removal preserve unaffected rows");

            int invoked = 0;
            page.ProjectTree.ItemInvoked += (_, args) =>
            {
                invoked++;
                if (args.InvokedItem is ProjectTreeEntry { Chat: { } chat })
                    page.DispatcherQueue.TryEnqueue(() => Update(chat.Id));
            };
            var container = (FrameworkElement)page.ProjectTree.ContainerFromItem(alphaChat);
            Check(container is not null, "active chat remains realized after archiving other rows");
            var peer = FrameworkElementAutomationPeer.CreatePeerForElement(container);
            var invoke = peer?.GetPattern(PatternInterface.Invoke) as IInvokeProvider;
            if (invoke is null) throw new InvalidOperationException("Native tree chat did not expose Invoke automation.");
            invoke.Invoke();
            await Settle(page);
            Check(invoked == 1 && alphaChat.IsActive, "native ItemInvoked completes before deferred reconciliation");

            for (int index = 0; index < 100; index++)
            {
                projects.Reverse();
                reloaded.Name = "Rapid update " + index;
                Update(index % 2 == 0 ? alphaChat.Chat!.Id : pinned.Chats[0].Id);
                if (index % 10 == 0) await Settle(page);
            }
            await Settle(page);
            Check(page.Entries.Single(row => row.Project.Id == reloaded.Id) == alphaRow && alphaRow.Children[0] == alphaChat,
                "100 rapid reorder and active-chat updates retain stable row identities");
            Check(resets == 0, "no collection reset occurs across lifecycle operations");

            int beforeRender = page.RenderPasses;
            for (int index = 0; index < 100; index++)
                page.QueueFixture(projects, index == 99 ? alphaChat.Chat : pinned.Chats[0]);
            Check(page.RenderPasses == beforeRender, "production renderer defers mutations until the current event completes");
            await Settle(page);
            Check(page.RenderPasses == beforeRender + 1 && alphaChat.IsActive && !page.IsRendering,
                "production renderer coalesces 100 queued updates and applies the latest active chat");
            Check(alphaRow.IsActive, "production renderer highlights the selected workspace independently of native tree selection");
            var replyingContainer = page.ProjectTree.ContainerFromItem(alphaChat);
            page.SetReplying(alphaChat.Chat!.Id, true);
            await Settle(page);
            Check(alphaChat.IsReplying && Descendants<FontIcon>((DependencyObject)replyingContainer).Any(icon =>
                    icon.Name == "ReplyingIndicator" && icon.Visibility == Visibility.Visible),
                "starting a reply updates its native status indicator without replacing the chat row");
            page.SetReplying(alphaChat.Chat!.Id, false);
            await Settle(page);
            Check(!alphaChat.IsReplying && ReferenceEquals(replyingContainer, page.ProjectTree.ContainerFromItem(alphaChat)) &&
                Descendants<FontIcon>((DependencyObject)replyingContainer).Any(icon =>
                    icon.Name == "ReplyingIndicator" && icon.Visibility == Visibility.Collapsed),
                "finishing a reply hides its indicator and preserves its native container");
            var projectContainer = (FrameworkElement)page.ProjectTree.ContainerFromItem(alphaRow);
            var projectPeer = FrameworkElementAutomationPeer.CreatePeerForElement(projectContainer);
            var expansion = projectPeer?.GetPattern(PatternInterface.ExpandCollapse) as IExpandCollapseProvider;
            if (expansion is null) throw new InvalidOperationException("Native project did not expose ExpandCollapse automation.");
            expansion.Collapse();
            await Settle(page);
            Check(page.WasCollapsedByUser(reloaded.Id) && !alphaRow.IsExpanded,
                "production collapsed event recognizes the current native project node");
            page.QueueFixture(projects, alphaChat.Chat, reloaded.Id);
            await Settle(page);
            Check(!alphaRow.IsExpanded, "production refresh respects native user collapse even when the active chat requests expansion");
            expansion.Expand();
            await Settle(page);
            Check(!page.WasCollapsedByUser(reloaded.Id) && alphaRow.IsExpanded,
                "production expanding event clears the manual-collapse override");
            for (int index = 0; index < 25; index++)
            {
                reloaded.IsPinned = !reloaded.IsPinned;
                projects.Reverse();
                page.QueueFixture(projects, alphaChat.Chat);
                await Settle(page);
            }
            Check(!page.WasCollapsedByUser(reloaded.Id) && alphaRow.IsExpanded,
                "native remove/insert during 25 queued reorderings never records a false manual collapse");
            Check(page.CheckUnloadedFocus(), "production hover callbacks ignore an unloaded row without querying a detached XamlRoot");
            await Settle(page);

            beforeRender = page.RenderPasses;
            page.QueueFixture(projects, pinned.Chats[0]);
            _window!.Content = new Grid();
            await Settle(page);
            Check(page.RenderPasses == beforeRender, "queued production render exits after page unload");
            _window.Content = page;
            await Settle(page);
            page.QueueFixture(projects, pinned.Chats[0]);
            page.CloseView();
            await Settle(page);
            Check(page.RenderPasses == beforeRender, "queued production render exits after window closing is marked");
            _successful = true;
            _window!.Close();
        }
        catch (Exception error)
        {
            File.WriteAllText(_result, "FAIL after " + _checks.Count + " checks: " + error);
            _window?.Close();
        }
    }

    private void Check(bool condition, string description)
    {
        if (!condition) throw new InvalidOperationException(description);
        _checks.Add("PASS: " + description);
    }

    private static ProjectChatState SavedChat(string title) => new()
    {
        Title = title,
        Messages = [new ChatMessageState { Content = "Saved fixture message" }]
    };

    private static async Task Settle(SmokePage page)
    {
        page.UpdateLayout();
        await Task.Delay(80);
    }

    private static IEnumerable<T> Descendants<T>(DependencyObject parent) where T : DependencyObject
    {
        for (int index = 0; index < VisualTreeHelper.GetChildrenCount(parent); index++)
        {
            var child = VisualTreeHelper.GetChild(parent, index);
            if (child is T value) yield return value;
            foreach (var descendant in Descendants<T>(child)) yield return descendant;
        }
    }
}
