using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private sealed class WorkspaceResources
    {
        public Dictionary<string, (long Order, long Revision, ConversationWorkTab Tab)> Items { get; } = new(StringComparer.Ordinal);
        public HashSet<string> ExaminedCalls { get; } = new(StringComparer.Ordinal);
        public long Touched { get; set; }
    }
    private readonly Dictionary<Guid, WorkspaceResources> _workspaceResources = [];
    private long _workspaceSequence;

    private static string ScreenshotTabKey(ConversationScreenshotSource source) =>
        $"screenshot/{source.ConversationId:D}/{source.MessageId:D}/{source.Tool.ToolCallId}";

    private void InitializeWorkTabs()
    {
        ScreenshotPanel.SetTabbedMode(true);
        WorkTabs.TabsChanged += (_, _) => { if (!_chatClosing) ApplyLayout(); };
        WorkTabs.TabInvoked += (_, tab) =>
        {
            if (!_chatClosing && ActiveChatId is Guid conversation && tab.Kind == "browser" && tab.Target is { } url)
                DocumentPanel.ShowWebsite(conversation, url, tab.Title);
        };
        WorkTabs.TabClosed += (_, closed) =>
        {
            if (closed.Tab.Kind == "terminal") WorkspaceTerminal.CloseSession(closed.ConversationId, closed.Tab.Key);
            else DocumentPanel.CloseTab(closed.ConversationId, closed.Tab);
        };
        WorkTabs.HomeRequested += (_, _) => { ApplyLayout(); WorkspaceHome.FocusAddress(); };
        WorkspaceHome.FileRequested += (_, _) => OpenWorkspaceFile();
        WorkspaceHome.WebsiteRequested += (_, url) => AddWebsiteTab(url);
        WorkspaceHome.ChangesRequested += (_, _) => OpenWorkspaceChanges();
        WorkspaceHome.TerminalRequested += (_, _) => OpenWorkspaceTerminal();
        WorkspaceTerminal.NewSessionRequested += (_, _) => OpenWorkspaceTerminal();
        ConversationMessages.AttachmentRequested += (_, request) =>
        {
            var attachment = request.Attachment;
            AddWorkspaceTab(request.ConversationId, new(
                $"attachment/{request.ConversationId:D}/{request.MessageId:D}/{attachment.Id:D}", "document", attachment.Name,
                $"attachment:{request.MessageId:D}/{attachment.Id:D}", attachment.Sha256), select: true);
        };
        TaskPanel.TasksChanged += (_, _) => UpdateTaskRegionLayout();
        TaskPanel.ArtifactOpenRequested += (_, request) => AcceptTaskArtifact(request, select: true);
        TaskPanel.ArtifactsChanged += (_, requests) =>
        {
            foreach (var request in requests) AcceptTaskArtifact(request, select: false);
        };
        DocumentPanel.WebsiteRequested += (_, url) => AddWebsiteTab(url);
        Unloaded += (_, _) => DocumentPanel.SetPreviewEnabled(false);
    }

    private WorkspaceResources ResourcesFor(Guid conversation)
    {
        if (!_workspaceResources.TryGetValue(conversation, out var resources)) _workspaceResources[conversation] = resources = new();
        resources.Touched = ++_workspaceSequence;
        while (_workspaceResources.Count > ConversationWorkTabState.MaximumConversations)
            _workspaceResources.Remove(_workspaceResources.Where(item => item.Key != conversation).MinBy(item => item.Value.Touched).Key);
        return resources;
    }

    private void SynchronizeWorkTabs()
    {
        var screenshots = new Dictionary<(Guid MessageId, string ToolCallId), ConversationWorkTab>();
        int imageNumber = 0;
        foreach (var source in ScreenshotPanel.Items)
            if (source.ConversationId == ActiveChatId)
                screenshots[(source.MessageId, source.Tool.ToolCallId)] =
                    new(ScreenshotTabKey(source), "screenshot", $"{UiText.Get("截图")} {++imageNumber}");
        var items = new List<ConversationWorkTab>();
        WorkspaceResources? resources = ActiveChatId is Guid conversation ? ResourcesFor(conversation) : null;
        foreach (var message in ActiveMessages)
            foreach (var tool in message.ToolActivities)
            {
                if (screenshots.Remove((message.Message.Id, tool.ToolCallId), out var tab)) items.Add(tab);
                if (resources is not null && ConversationWorkspaceSources.IsResourceTool(tool.Name) && tool.Status == "completed")
                {
                    string identity = $"{message.Message.Id:D}/{tool.ToolCallId}/{tool.ResultRef?.Id:D}";
                    if (resources.ExaminedCalls.Add(identity))
                        _ = DiscoverWorkspaceResourceAsync(message.ConversationId, tool, resources, ++_workspaceSequence,
                            _pendingReplies.TryGetValue(message.ConversationId, out var pending) && pending.Message.Id == message.Message.Id);
                }
            }
        // Live receipts may precede the next message projection; an existing selection stays stable.
        // 实时回执可能先于消息投影到达；新产物在后台加入，不抢占已有选择。
        items.AddRange(screenshots.Values);
        if (resources is not null) items.AddRange(resources.Items.Values.OrderBy(item => item.Order).Select(item => item.Tab));
        WorkTabs.ShowConversation(ActiveChatId, items);
    }

    private async Task DiscoverWorkspaceResourceAsync(Guid conversation, ToolActivity tool, WorkspaceResources resources, long order, bool liveReply)
    {
        try
        {
            // Historical receipts may be large; only immutable receipt reads/projection run off the UI thread.
            // 历史回执可能很大；后台只读取及投影不可变回执，缓存和标签更新仍留在界面线程。
            var tab = await Task.Run(async () =>
            {
                ConversationWorkTab? discovered = null;
                if (!string.IsNullOrWhiteSpace(tool.Result))
                {
                    try
                    {
                        using var result = JsonDocument.Parse(tool.Result);
                        discovered = ConversationWorkspaceSources.FromTool(tool, result.RootElement);
                    }
                    catch (JsonException) { /* A truncated preview is not the archived receipt. 中文：截断预览不替代归档回执。 */ }
                }
                if (discovered is null && tool.ResultRef is { Bytes: > 0 and <= 2 * 1024 * 1024 } reference)
                {
                    var result = await _agentApiClient.GetToolResultAsync(conversation, reference).ConfigureAwait(false);
                    discovered = ConversationWorkspaceSources.FromTool(tool, result.Result);
                }
                return discovered;
            });
            if (_chatClosing || tab is null) return;
            StoreWorkspaceResource(resources, tab, order);
            if (ActiveChatId == conversation)
            {
                // A live presentation request opens its page; historical receipts restore metadata only.
                // 实时展示回执打开网页；历史回执只恢复标签信息，不能在软件启动时拉起浏览器。
                if (tab.Kind == "browser" && liveReply &&
                    tool.Name is "artifact.open" or "browser.open" or "computer.launch")
                    AddWorkspaceTab(conversation, tab, select: true);
                else ApplyLayout();
            }
        }
        catch (Exception error) when (error is HttpRequestException or InvalidDataException or InvalidOperationException or TaskCanceledException or JsonException)
        {
            // Preview discovery never changes task or conversation execution.
            // 预览发现失败只影响右侧展示，不改变任务和对话执行。
        }
    }

    private void AcceptTaskArtifact(TaskArtifactRequest request, bool select)
    {
        if (_chatClosing || request.ConversationId != ActiveChatId) return;
        var tab = ConversationWorkspaceSources.FromReference(request.Artifact.Reference);
        if (tab is null) return;
        tab = tab with { Version = tab.Version ?? request.Artifact.SourceVersion ?? request.Artifact.Sha256 };
        AddWorkspaceTab(request.ConversationId, tab, select);
    }

    private void AddWorkspaceTab(Guid conversation, ConversationWorkTab tab, bool select)
    {
        StoreWorkspaceResource(ResourcesFor(conversation), tab, ++_workspaceSequence);
        if (ActiveChatId != conversation) return;
        if (select) { _layout.PreviewVisible = true; _emptyWorkPanelRequested = true; }
        ApplyLayout();
        if (select)
        {
            WorkTabs.Reopen(tab.Key);
            if (tab.Kind == "browser" && tab.Target is { } url) DocumentPanel.ShowWebsite(conversation, url, tab.Title);
        }
    }

    private static void StoreWorkspaceResource(WorkspaceResources resources, ConversationWorkTab tab, long revision)
    {
        if (resources.Items.TryGetValue(tab.Key, out var previous))
        {
            if (revision >= previous.Revision) resources.Items[tab.Key] = (previous.Order, revision, tab);
        }
        else resources.Items[tab.Key] = (revision, revision, tab);
    }

    private void PresentSelectedWorkTab(bool sidebarVisible)
    {
        var selected = WorkTabs.SelectedTab;
        var screenshot = selected?.Kind == "screenshot"
            ? ScreenshotPanel.Items.FirstOrDefault(source => ScreenshotTabKey(source) == selected.Key) : null;
        bool showScreenshot = sidebarVisible && screenshot is not null;
        bool showDocument = sidebarVisible && ActiveChatId is not null && selected?.Target is not null &&
            selected.Kind is "document" or "diagram" or "browser" or "changes";
        bool showTerminal = sidebarVisible && ActiveChatId is not null && selected?.Kind == "terminal";

        if (!showScreenshot) ScreenshotPanel.SetPreviewEnabled(false);
        if (!showDocument) DocumentPanel.SetPreviewEnabled(false);
        if (!showTerminal) WorkspaceTerminal.SetPreviewEnabled(false);
        if (showScreenshot)
        {
            if (ScreenshotPanel.SelectedSource?.Identity != screenshot!.Identity)
                ScreenshotPanel.SelectSource(screenshot.MessageId, screenshot.Tool.ToolCallId);
            ScreenshotPanel.SetPreviewEnabled(true);
        }
        if (showDocument)
        {
            if (selected!.Target!.StartsWith("attachment:", StringComparison.Ordinal))
            {
                var ids = selected.Target[11..].Split('/');
                if (ids.Length == 2 && Guid.TryParse(ids[0], out var messageId) && Guid.TryParse(ids[1], out var attachmentId)
                    && ActiveMessages.FirstOrDefault(row => row.Message.Id == messageId)?.Message.Attachments
                        .FirstOrDefault(attachment => attachment.Id == attachmentId) is { } attachment)
                    DocumentPanel.ShowAttachment(ActiveChatId!.Value, messageId, attachment);
            }
            else if (selected.Kind == "changes") DocumentPanel.ShowChanges(ActiveChatId!.Value);
            else if (selected.Kind == "browser") DocumentPanel.ShowWebsite(ActiveChatId!.Value, selected.Target!, selected.Title, activate: false);
            else DocumentPanel.ShowResource(ActiveChatId!.Value, selected.Target!, selected.Title, selected.UserSelected, selected.Version);
            DocumentPanel.SetPreviewEnabled(true);
        }
        if (showTerminal)
        {
            WorkspaceTerminal.ShowSession(ActiveChatId!.Value, selected!.Key);
            WorkspaceTerminal.SetPreviewEnabled(true);
        }
        ScreenshotPanel.Visibility = showScreenshot ? Visibility.Visible : Visibility.Collapsed;
        DocumentPanel.Visibility = showDocument ? Visibility.Visible : Visibility.Collapsed;
        WorkspaceTerminal.Visibility = showTerminal ? Visibility.Visible : Visibility.Collapsed;
        WorkspaceHome.ShowDirectory(CurrentWorkspaceDirectory(), ActiveChatId is not null);
        WorkspaceHome.Visibility = sidebarVisible && !showScreenshot && !showDocument && !showTerminal ? Visibility.Visible : Visibility.Collapsed;
        UpdateTaskRegionLayout();
    }

    private void SuspendUnselectedWorkViewers(bool sidebarVisible)
    {
        var selected = WorkTabs.SelectedTab;
        var source = ScreenshotPanel.SelectedSource;
        if (!sidebarVisible || source is null || source.ConversationId != ActiveChatId || selected?.Key != ScreenshotTabKey(source))
            ScreenshotPanel.SetPreviewEnabled(false);
        if (!sidebarVisible || selected?.Kind is not ("document" or "diagram" or "browser" or "changes")) DocumentPanel.SetPreviewEnabled(false);
        if (!sidebarVisible || selected?.Kind != "terminal") WorkspaceTerminal.SetPreviewEnabled(false);
    }
}
