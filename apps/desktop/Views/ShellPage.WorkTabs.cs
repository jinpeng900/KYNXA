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
        public HashSet<string> ActivatedWebsites { get; } = new(StringComparer.Ordinal);
        public Dictionary<string, ChatAttachment> Attachments { get; } = new(StringComparer.Ordinal);
        public long Touched { get; set; }
    }
    private readonly Dictionary<Guid, WorkspaceResources> _workspaceResources = [];
    private long _workspaceSequence;

    private static string ScreenshotTabKey(ConversationScreenshotSource source) =>
        source.TabKey;

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
        WorkspaceHome.MountRequested += (_, mountId) => SelectWorkspaceMount(mountId);
        WorkspaceHome.DirectoryRequested += async (_, path) => await BrowseWorkspaceFilesAsync(path);
        WorkspaceHome.WorkspaceFileRequested += (_, path) =>
        {
            if (!_chatClosing && ActiveChatId is Guid conversation)
                  AddWorkspaceTab(conversation, ConversationWorkspaceSources.File(path, userSelected: true) with { MountId = CurrentWorkspaceMount()?.Id }, select: true);
        };
        WorkspaceHome.FileBrowserClosed += (_, _) => CancelWorkspaceFileRead(forgetScope: true);
        WorkspaceHome.RefreshRequested += async (_, _) =>
        {
            if (WorkspaceHome.IsShowingAttachments) await BrowseSavedAttachmentsAsync(_attachmentOffset);
            else await BrowseWorkspaceFilesAsync(_filesPath);
        };
        WorkspaceHome.AttachmentsRequested += async (_, _) => await BrowseSavedAttachmentsAsync();
        WorkspaceHome.AttachmentPageRequested += async (_, offset) => await BrowseSavedAttachmentsAsync(offset);
        WorkspaceHome.SavedAttachmentRequested += (_, entry) => OpenSavedWorkspaceAttachment(entry);
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
        ConversationMessages.LinkRequested += (_, request) => OpenMarkdownLink(request.ConversationId, request.Target);
        DocumentPanel.FileRequested += (_, request) => OpenMarkdownLink(request.ConversationId, request.Path, request.MountId);
        Unloaded += (_, _) => { DocumentPanel.SetPreviewEnabled(false); CancelWorkspaceFileRead(forgetScope: true); };
    }

    private void OpenMarkdownLink(Guid conversation, string target, Guid? mountId = null)
    {
        if (_chatClosing || ActiveChatId != conversation || !TranscriptMarkdown.IsSafeUrl(target)) return;
        if (Uri.TryCreate(target, UriKind.Absolute, out var address) && address.Scheme is "http" or "https")
        { AddWebsiteTab(address.AbsoluteUri); return; }
        int marker = target.IndexOf('#');
        string path = Uri.UnescapeDataString(marker < 0 ? target : target[..marker]);
        if (string.IsNullOrWhiteSpace(path)) return;
        AddWorkspaceTab(conversation, ConversationWorkspaceSources.File(path, userSelected: true) with
            { MountId = mountId ?? CurrentWorkspaceMount()?.Id }, select: true);
        if (marker >= 0) DocumentPanel.NavigateToAnchor(target[(marker + 1)..]);
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
        var screenshots = new Dictionary<Guid, ConversationWorkTab>();
        int imageNumber = 0;
        foreach (var source in ScreenshotPanel.Items)
            if (source.ConversationId == ActiveChatId)
                screenshots[source.Tool.ResultRef!.Id] =
                    new(ScreenshotTabKey(source), "screenshot", string.IsNullOrWhiteSpace(source.Title) ? $"{UiText.Get("截图")} {++imageNumber}" : source.Title,
                        PresentationPriority: source.PresentationPriority);
        var items = new List<ConversationWorkTab>();
        WorkspaceResources? resources = ActiveChatId is Guid conversation ? ResourcesFor(conversation) : null;
        foreach (var message in ActiveMessages)
            foreach (var tool in message.ToolActivities)
            {
                if (tool.ResultRef is { } screenshotRef && screenshots.Remove(screenshotRef.Id, out var tab)) items.Add(tab);
                if (resources is not null)
                    QueueWorkspaceResourceDiscovery(message.ConversationId, message.Message.Id, tool,
                        _pendingReplies.TryGetValue(message.ConversationId, out var pending) && pending.Message.Id == message.Message.Id);
            }
        // Live receipts may precede the next message projection; an existing selection stays stable.
        // 实时回执可能先于消息投影到达；新产物在后台加入，不抢占已有选择。
        items.AddRange(screenshots.Values);
        if (resources is not null) items.AddRange(resources.Items.Values.OrderBy(item => item.Order).Select(item => item.Tab));
        string? finalAnswer = ActiveMessages.LastOrDefault(message => message.Message.Role == "assistant" &&
            message.Message.Status == "completed")?.Message.Content;
        WorkTabs.ShowConversation(ActiveChatId, ConversationWorkspaceSources.PrioritizeDeliveredOutputs(items, finalAnswer),
            catalogComplete: ScreenshotPanel.IsSourceCatalogReady);
    }

    private void QueueWorkspaceResourceDiscovery(Guid conversation, Guid messageId, ToolActivity tool, bool liveReply)
    {
        if (_chatClosing || tool.Status != "completed" || !ConversationWorkspaceSources.IsResourceTool(tool.Name)) return;
        var resources = ResourcesFor(conversation);
        string identity = $"{messageId:D}/{tool.ToolCallId}/{tool.ResultRef?.Id:D}";
        if (resources.ExaminedCalls.Add(identity))
            _ = DiscoverWorkspaceResourceAsync(conversation, tool, resources, ++_workspaceSequence, liveReply);
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
            tab = LabelWorkspaceMount(conversation, tab);
            StoreWorkspaceResource(resources, tab, order);
            if (tab.Kind == "browser" && liveReply) resources.ActivatedWebsites.Add(tab.Key);
            if (ActiveChatId == conversation)
            {
                // Live presentation may reveal the workspace, but must not replace a tab the user is reading.
                // 实时展示可展开工作区，但不能换掉用户正在阅读的标签；历史恢复仍不启动浏览器。
                if (tab.Kind == "browser" && liveReply &&
                    tool.Name is "artifact.open" or "browser.open" or "computer.launch")
                { _layout.PreviewVisible = true; _emptyWorkPanelRequested = true; }
                ApplyLayout();
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

    private void AcceptWorkspaceTerminal(Guid conversation, ChatStreamEvent update)
    {
        if (_chatClosing || update.SessionId is not { } sessionId || update.Cols is not int cols || update.Rows is not int rows) return;
        string tabKey = WorkspaceTerminal.AttachSession(conversation, new(sessionId, cols, rows), update.MountId);
        var resources = ResourcesFor(conversation);
        string title = update.Title ?? UiText.Get("终端");
        if (!string.IsNullOrWhiteSpace(update.MountName)) title += " · " + update.MountName;
        var tab = ConversationWorkspaceSources.Terminal(conversation, sessionId, title) with { Key = tabKey, MountId = update.MountId };
        if (resources.Items.TryGetValue(tabKey, out var previous) && string.IsNullOrWhiteSpace(update.Title))
            tab = tab with { Title = previous.Tab.Title };
        StoreWorkspaceResource(resources, tab, ++_workspaceSequence);
        if (ActiveChatId != conversation) return;
        _layout.PreviewVisible = true; _emptyWorkPanelRequested = true;
        ApplyLayout();
    }

    private void AddWorkspaceTab(Guid conversation, ConversationWorkTab tab, bool select)
    {
        tab = LabelWorkspaceMount(conversation, tab);
        var resources = ResourcesFor(conversation);
        StoreWorkspaceResource(resources, tab, ++_workspaceSequence);
        if (select && tab.Kind == "browser") resources.ActivatedWebsites.Add(tab.Key);
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
            if (revision >= previous.Revision) resources.Items[tab.Key] = (previous.Order, revision, tab with
            {
                UserSelected = previous.Tab.UserSelected || tab.UserSelected,
                PresentationPriority = previous.Tab.PresentationPriority > tab.PresentationPriority
                    ? previous.Tab.PresentationPriority : tab.PresentationPriority
            });
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
                    && FindWorkspaceAttachment(ActiveChatId!.Value, messageId, attachmentId) is { } attachment)
                    DocumentPanel.ShowAttachment(ActiveChatId!.Value, messageId, attachment);
            }
            else if (selected.Kind == "changes") DocumentPanel.ShowChanges(ActiveChatId!.Value, selected.MountId);
            else if (selected.Kind == "browser")
            {
                // Retain this runtime's explicit/live opening intent across layout refreshes and chat switches.
                // 本次运行中的明确打开或实时展示跨布局刷新、聊天切换保留；历史恢复仍不自动启动。
                DocumentPanel.ShowWebsite(ActiveChatId!.Value, selected.Target!, selected.Title,
                    activate: ResourcesFor(ActiveChatId.Value).ActivatedWebsites.Contains(selected.Key));
            }
            else DocumentPanel.ShowResource(ActiveChatId!.Value, selected.Target!, selected.Title, selected.UserSelected, selected.Version, selected.MountId);
            DocumentPanel.SetPreviewEnabled(true);
        }
        if (showTerminal)
        {
            WorkspaceTerminal.ShowSession(ActiveChatId!.Value, selected!.Key, selected.MountId);
            WorkspaceTerminal.SetPreviewEnabled(true);
        }
        ScreenshotPanel.Visibility = showScreenshot ? Visibility.Visible : Visibility.Collapsed;
        DocumentPanel.Visibility = showDocument ? Visibility.Visible : Visibility.Collapsed;
        WorkspaceTerminal.Visibility = showTerminal ? Visibility.Visible : Visibility.Collapsed;
        WorkspaceHome.ShowDirectory(CurrentWorkspaceDirectory(), ActiveChatId is not null);
        WorkspaceHome.ShowMounts(CurrentWorkspaceProject()?.Mounts ?? [], CurrentWorkspaceMount()?.Id);
        WorkspaceHome.ShowProjectScope(CurrentWorkspaceProject() is not null);
        if (_filesConversation != ActiveChatId || _filesBinding != CurrentWorkspaceFilesBinding())
        { CancelWorkspaceFileRead(forgetScope: true); WorkspaceHome.HideFiles(); }
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
