using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;

namespace KYNXA_Desktop;

public partial class App
{
    private async Task CheckWorkspaceResourcesAsync()
    {
        // Exercise the real controls in the existing window; directory receipts remain synthetic and scoped.
        // 在原有窗口承载真实控件；目录回执使用范围固定的虚构数据，不读取用户文件。
        var root = (Grid)_shell.Content;
        var home = new ConversationWorkspaceHome();
        var usage = new ConversationUsageSummary();
        var host = new Grid { Width = 360, Height = 600, HorizontalAlignment = HorizontalAlignment.Left,
            VerticalAlignment = VerticalAlignment.Top, Margin = new Thickness(20),
            Background = AppearanceService.GetBrush("KynxaMainBrush") };
        host.RowDefinitions.Add(new() { Height = new GridLength(1, GridUnitType.Star) });
        host.RowDefinitions.Add(new() { Height = GridLength.Auto });
        host.Children.Add(home); host.Children.Add(usage); Grid.SetRow(usage, 1);
        Grid.SetColumnSpan(host, Math.Max(1, root.ColumnDefinitions.Count));
        Grid.SetRowSpan(host, Math.Max(1, root.RowDefinitions.Count)); Canvas.SetZIndex(host, 100);
        var windows = PresentationTrackedWindows();
        var popups = VisualTreeHelper.GetOpenPopupsForXamlRoot(_shell.XamlRoot).Where(popup => popup.IsOpen).ToArray();
        var delayedDirectory = new TaskCompletionSource<WorkspaceDirectoryListing>(TaskCreationOptions.RunContinuationsAsynchronously);
        var directoryEntered = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        CancellationToken oldDirectoryToken = default;
        int directoryReads = 0;
        string mount = Path.Combine(_directory, "Workspace");
        string oldPath = Path.Combine(mount, "pending");
        SavedAttachmentEntry? opened = null;
        int? requestedOffset = null;
        var scopes = new List<bool>();
        home.SavedAttachmentRequested += (_, entry) => opened = entry;
        home.AttachmentPageRequested += (_, offset) => requestedOffset = offset;
        home.AttachmentsRequested += (_, project) => scopes.Add(project);
        root.Children.Add(host);
        try
        {
            await WaitAsync(() => home.IsLoaded && usage.IsLoaded && host.ActualWidth > 0,
                "the resource controls load in the existing production Shell XamlRoot");
            home.ShowDirectory(mount, hasConversation: true); home.ShowProjectScope(true);
            home.DirectoryLoader = async (path, token) =>
            {
                directoryReads++;
                if (path == oldPath)
                {
                    oldDirectoryToken = token; directoryEntered.TrySetResult();
                    // Simulate an old provider that completes after cancellation to verify generation fencing too.
                    // 模拟旧提供方取消后仍返回，以同时核验过期结果隔离，而非仅依赖取消配合。
                    return await delayedDirectory.Task;
                }
                return new WorkspaceDirectoryListing(path, [new("nested.md", "file")]);
            };
            home.ShowFilesLoading();
            home.ShowFiles(new(mount, [new("details", "directory"), new("pending", "directory"),
                new("a-very-long-workspace-file-name-that-must-stay-inside-a-narrow-pane.md", "file")]), mount);
            await SettleAsync();
            var details = NativeUi.ById<Expander>(home, "WorkspaceFolder_details");
            var pending = NativeUi.ById<Expander>(home, "WorkspaceFolder_pending");
            Check(directoryReads == 0 && home.ActualWidth <= 361 && details.ActualWidth <= home.ActualWidth,
                "a 360px workspace renders folder branches without eagerly reading their contents");
            details.IsExpanded = true;
            await WaitAsync(() => NativeUi.Descendants<Button>(home).Any(button =>
                AutomationProperties.GetAutomationId(button) == "WorkspaceFile_nested.md" && NativeUi.IsVisible(button)),
                "an expanded branch renders its scoped receipt");
            details.IsExpanded = false; await SettleAsync(); details.IsExpanded = true; await SettleAsync();
            Check(directoryReads == 1 && NativeUi.Descendants<Button>(home).Count(button =>
                AutomationProperties.GetAutomationId(button) == "WorkspaceFile_nested.md") == 1,
                "reopening the same branch reuses its loaded content without another read or duplicate file row");

            pending.IsExpanded = true;
            await directoryEntered.Task.WaitAsync(TimeSpan.FromSeconds(5));
            home.ShowAttachmentsLoading();
            delayedDirectory.TrySetResult(new(oldPath, [new("stale-must-not-appear.md", "file")]));
            await SettleAsync();
            Check(oldDirectoryToken.IsCancellationRequested && home.IsShowingAttachments &&
                !NativeUi.Descendants<Button>(home).Concat(NativeUi.Descendants<Button>(pending))
                    .Any(button => AutomationProperties.GetAutomationId(button) == "WorkspaceFile_stale-must-not-appear.md"),
                "switching resource scope cancels an old branch and rejects even its late successful result");

            var chatEntry = new SavedAttachmentEntry(Guid.NewGuid(), "chat-only.docx", "application/octet-stream", 15,
                _gateway.WorkChatId, Guid.NewGuid(), "Current synthetic chat", ProjectId: _gateway.ProjectId);
            home.ShowAttachments(new([chatEntry], Total: 1)); await SettleAsync();
            NativeUi.Invoke(NativeUi.ById<Button>(home, "SavedAttachment_" + chatEntry.Id.ToString("D")));
            Check(!home.UsesProjectAttachments && opened == chatEntry,
                "chat attachment activation preserves the exact attachment, source conversation and message identities");

            NativeUi.ById<RadioButton>(home, "Segment_work").IsChecked = true;
            var workEntry = chatEntry with { Id = Guid.NewGuid(), Name = "other-chat.pptx", ConversationId = Guid.NewGuid(), MessageId = Guid.NewGuid() };
            home.ShowAttachments(new([workEntry], Limit: 50, Total: 51, HasMore: true, NextOffset: 50)); await SettleAsync();
            NativeUi.Invoke(NativeUi.ById<Button>(home, "SavedAttachment_" + workEntry.Id.ToString("D")));
            NativeUi.Invoke(NativeUi.Descendants<Button>(home).Single(button => Equals(button.Tag, "下一页")));
            Check(scopes.SequenceEqual([true]) && home.UsesProjectAttachments && opened == workEntry &&
                opened.ConversationId != chatEntry.ConversationId && requestedOffset == 50,
                "project scope emits its explicit selection and pagination retains the selected attachment's original chat ownership");

            var nextEntry = workEntry with { Id = Guid.NewGuid(), Name = "next-page.xlsx", MessageId = Guid.NewGuid() };
            home.ShowAttachments(new([nextEntry], Offset: 50, Limit: 50, Total: 51)); await SettleAsync();
            NativeUi.Invoke(NativeUi.ById<Button>(home, "SavedAttachment_" + nextEntry.Id.ToString("D")));
            NativeUi.Invoke(NativeUi.Descendants<Button>(home).Single(button => Equals(button.Tag, "上一页")));
            Check(opened == nextEntry && requestedOffset == 0 && !NativeUi.Descendants<Button>(home).Any(button =>
                AutomationProperties.GetAutomationId(button) == "SavedAttachment_" + workEntry.Id.ToString("D")),
                "changing pages replaces old attachment rows and previous-page navigation returns the matching offset");
            NativeUi.ById<RadioButton>(home, "Segment_chat").IsChecked = true; home.ShowProjectScope(false);
            Check(scopes.SequenceEqual([true, false]) && !home.UsesProjectAttachments &&
                !NativeUi.IsVisible(NativeUi.ById<RadioButton>(home, "Segment_work")),
                "chat-only scope cannot retain or display an unrelated project attachment selection");

            usage.Show(new("conversation", _gateway.WorkChatId, _gateway.ProjectId,
                4, 100, 20, 3, 1, 1, 50, 100, 1, 0.5, 0)); await SettleAsync();
            double compactHeight = usage.ActualHeight;
            var toggle = NativeUi.ById<Button>(usage, "ConversationUsageDetailsToggle");
            NativeUi.Invoke(toggle); await SettleAsync();
            bool WithinWidth(FrameworkElement element)
            {
                var bounds = element.TransformToVisual(usage).TransformBounds(new Rect(0, 0, element.ActualWidth, element.ActualHeight));
                return bounds.Left >= -1 && bounds.Right <= usage.ActualWidth + 1;
            }
            Check(usage.IsDetailsExpanded && usage.ActualHeight > compactHeight && usage.ActualWidth <= 361 &&
                WithinWidth(toggle) && NativeUi.Descendants<TextBlock>(usage).Where(NativeUi.IsVisible).All(WithinWidth) &&
                toggle.CornerRadius.TopLeft >= 8 && toggle.FocusVisualPrimaryBrush is SolidColorBrush focus &&
                focus.Color == AppearanceService.GetBrush("KynxaSecondaryTextBrush").Color,
                "usage expands inline at 360px with bounded text, rounded controls and a neutral focus brush");
            usage.Reset(); usage.Show(new("conversation", _gateway.WorkChatId, _gateway.ProjectId,
                1, 10, 4, 1, 0, 0, 0, 0, 0, null, 0)); await SettleAsync();
            Check(!usage.IsDetailsExpanded && NativeUi.Descendants<TextBlock>(usage).Any(text => text.Text.Contains('—')) &&
                !NativeUi.Descendants<TextBlock>(usage).Any(text => text.Text.Contains("0%", StringComparison.Ordinal)),
                "a scope reset collapses old usage details and missing cache measurements remain unknown");
            Check(ReferenceEquals(home.XamlRoot, _shell.XamlRoot) && ReferenceEquals(usage.XamlRoot, _shell.XamlRoot) &&
                windows.ToHashSet().SetEquals(PresentationTrackedWindows()) &&
                popups.ToHashSet().SetEquals(VisualTreeHelper.GetOpenPopupsForXamlRoot(_shell.XamlRoot).Where(popup => popup.IsOpen)),
                "resource navigation and usage disclosure remain in the existing window without a new popup or tracked window");
        }
        finally
        {
            home.HideFiles(); delayedDirectory.TrySetCanceled(); root.Children.Remove(host);
        }
    }
}
