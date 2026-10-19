using System.Collections;
using System.Reflection;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using KYNXA_Desktop.Views;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;

namespace KYNXA_Desktop;

public partial class App
{
    private ContentDialog CreateModalGuardFixture() => DialogPresentation.Prepare(new ContentDialog
    {
        XamlRoot = _shell.XamlRoot, Title = "Generic modal fixture", Content = "Synthetic modal focus and theme check.",
        DefaultButton = ContentDialogButton.None, PrimaryButtonText = "Continue", CloseButtonText = "Cancel",
        PrimaryButtonStyle = (Style)Current.Resources["KynxaQuietButtonStyle"],
        CloseButtonStyle = (Style)Current.Resources["KynxaQuietButtonStyle"]
    });

    private ToolApprovalCard ApprovalCard => NativeUi.ById<ToolApprovalCard>(_shell, "ToolApprovalCard");
    private Button ApprovalButton(string id) => NativeUi.ById<Button>(ApprovalCard, id);
    private TextBlock ApprovalSummary => NativeUi.ById<TextBlock>(ApprovalCard, "ToolApprovalSummary");
    private bool ApprovalVisible => ApprovalCard.Visibility == Visibility.Visible && NativeUi.IsVisible(ApprovalCard);
    private static Rect RelativeBounds(FrameworkElement element, FrameworkElement root) =>
        element.TransformToVisual(root).TransformBounds(new Rect(0, 0, element.ActualWidth, element.ActualHeight));

    private async Task CheckInlineApprovalAsync()
    {
        _gateway.EnableApprovals = true;
        string palette = AppearanceService.Current.Id, mode = AppearanceService.Mode;
        string? accent = AppearanceService.CustomAccent;
        double fontSize = AppearanceService.ChatFontSize;
        var pendingReplies = ShellField<IDictionary>("_pendingReplies");
        var owned = new List<(Guid ConversationId, object Pending)>();
        var queue = Element<ConversationPromptQueue>("PromptQueue");
        var composer = Element<ComposerSurface>("ComposerHost");
        Guid childTask = Guid.NewGuid();
        string permissionMode = ShellField<LayoutState>("_layout").PermissionMode;
        var geometry = new List<object>();

        object CreatePending(Guid conversationId)
        {
            // Own only the pending presentation; no ReceiveReplyAsync, model stream or real tool runs.
            // 仅创建自有待回复展示；不调用 ReceiveReplyAsync、模型流或真实工具。
            Type type = typeof(ShellPage).GetNestedType("PendingChatReply", BindingFlags.NonPublic)!;
            var pending = Activator.CreateInstance(type, [conversationId, Guid.NewGuid(), "Synthetic approval task",
                "fixture-provider", "fixture-model", "ask", null])!;
            var message = (ChatMessageState)type.GetProperty("Message")!.GetValue(pending)!;
            type.GetProperty("Presentation")!.SetValue(pending, new ConversationMessageViewModel(conversationId, message));
            pendingReplies.Add(conversationId, pending);
            owned.Add((conversationId, pending));
            Call("UpdateSendButtonState");
            return pending;
        }
        Guid RequestId(object pending) => ((ChatMessageState)pending.GetType().GetProperty("Message")!.GetValue(pending)!).Id;
        ToolActivity BrowserOperation(string call, string host) => new(call, "browser.open",
            JsonSerializer.SerializeToElement(new { url = "https://" + host + "/synthetic/path", reason = "Synthetic explanation" }),
            "approval-required", "Waiting for authorization", ApprovalId: Guid.NewGuid());
        void Queue(object pending, ToolActivity tool, Guid? taskId = null) => Call("QueueToolApproval", pending, tool, taskId);
        void CheckRequest(int index, object pending, ToolActivity tool, bool approved, string scope)
        {
            var request = _gateway.ApprovalRequests[index];
            Check(request.ConversationId == (Guid)pending.GetType().GetProperty("ConversationId")!.GetValue(pending)! &&
                request.RequestId == RequestId(pending) && request.ToolCallId == tool.ToolCallId &&
                request.ApprovalId == tool.ApprovalId && request.Approved == approved && request.Scope == scope,
                $"approval receipt {index + 1} preserves the exact conversation, request, call, approval identity and {scope} decision");
        }
        async Task WaitCardAsync(string host) => await WaitAsync(() => ApprovalVisible && ApprovalSummary.Text.Contains(host),
            "the inline card presents " + host + " on its owning conversation");
        async Task CheckGeometryAsync(string label)
        {
            await SettleAsync();
            Check(ApprovalCard.Content is Border surface &&
                ReferenceEquals(surface.Background, AppearanceService.GetBrush("KynxaSettingsCardBrush")) &&
                ReferenceEquals(surface.BorderBrush, AppearanceService.GetBrush("KynxaDividerBrush")) &&
                ReferenceEquals(ApprovalSummary.Foreground, AppearanceService.GetBrush("KynxaTextBrush")),
                label + " presents the shared current theme surface, border and readable summary brushes");
            var bounds = RelativeBounds(ApprovalCard, _shell);
            var input = RelativeBounds(composer, _shell);
            Check(bounds.Width > 0 && bounds.Width < input.Width &&
                Math.Abs(bounds.X + bounds.Width / 2 - input.X - input.Width / 2) < 1.5 &&
                bounds.Bottom <= input.Top + 1 && bounds.Top >= 0,
                label + " keeps the real card centered, narrower than and above the composer within the viewport");
            var actions = new[] { "ApproveAllButton", "ApproveOnceButton", "RejectApprovalButton" }
                .Select(ApprovalButton).Select(button => RelativeBounds(button, ApprovalCard)).ToArray();
            Check(actions.All(rect => rect.Width >= 50 && rect.Height >= 30 && rect.Left >= 0 && rect.Top >= 0 &&
                rect.Right <= ApprovalCard.ActualWidth + 1 && rect.Bottom <= ApprovalCard.ActualHeight + 1),
                label + " keeps all three actual decision buttons readable and unclipped");
            Check(((SolidColorBrush)ApprovalButton("ApproveOnceButton").Background).Color ==
                ((SolidColorBrush)AppearanceService.GetBrush("KynxaSendBrush")).Color,
                label + " uses the application accent for approval instead of the system default blue");
            geometry.Add(new { Label = label, Card = bounds, Composer = input, Actions = actions });
            await CapturePresentedAsync("shell-inline-approval-" + label + ".png");
        }

        try
        {
            await SearchAndSelectAsync(_gateway.WorkChatId, "Work fixture");
            object pendingA = CreatePending(_gateway.WorkChatId);
            NativeUi.SetText(Prompt, "Synthetic queued first");
            Check((bool)Call("TryQueuePrompt")!, "the production composer queues its first owned prompt");
            NativeUi.SetText(Prompt, "Synthetic queued second");
            Check((bool)Call("TryQueuePrompt")!, "the production composer queues its second owned prompt");
            var prompts = ((IEnumerable)Call("QueueFor", _gateway.WorkChatId)!).Cast<QueuedPrompt>().ToArray();
            await SettleAsync();
            double originalQueueHeight = queue.ActualHeight;
            Rect originalComposer = RelativeBounds(composer, _shell);
            NativeUi.SetText(Prompt, "Draft remains editable during approval");
            await ActivateOwnedShortcutWindowAsync();
            await SettleAsync();
            Prompt.Focus(FocusState.Programmatic);
            await SettleAsync();
            Prompt.SelectionStart = 6;
            Prompt.SelectionLength = 7;
            object? originalFocus = FocusManager.GetFocusedElement(_shell.XamlRoot);
            Check(ReferenceEquals(originalFocus, Prompt), "the owned composer has settled focus before injecting an approval");
            var first = BrowserOperation("main-inline-call", "bilibili.com");
            Queue(pendingA, first);
            Queue(pendingA, first);
            await WaitCardAsync("bilibili.com");
            await SettleAsync();
            File.WriteAllText(Path.Combine(_directory, "shell-inline-approval-arrival.json"),
                JsonSerializer.Serialize(new { Original = (originalFocus as FrameworkElement)?.Name,
                    Current = (FocusManager.GetFocusedElement(_shell.XamlRoot) as FrameworkElement)?.Name,
                    Prompt.SelectionStart, Prompt.SelectionLength, Modal = (bool)Call("HasPresentationModal")!,
                    Dialog = NativeUi.OpenDialog(_shell)?.GetType().Name }));
            Check(NativeUi.OpenDialog(_shell) is null && !(bool)Call("HasPresentationModal")! &&
                ReferenceEquals(originalFocus, FocusManager.GetFocusedElement(_shell.XamlRoot)) &&
                Prompt.SelectionStart == 6 && Prompt.SelectionLength == 7,
                "arrival does not create a ContentDialog, claim modal status, steal input focus or alter its selection");
            Check(!ApprovalSummary.Text.Contains('\n') && !ApprovalSummary.Text.Contains("reason") &&
                !ApprovalSummary.Text.Contains('{') && !NativeUi.Descendants<TextBlock>(ApprovalCard)
                    .Any(text => text.Name == "ToolApprovalDetails" && NativeUi.IsVisible(text)),
                "the default presentation is one concise action sentence with technical details collapsed");
            Check(queue.Opacity == 0 && !queue.IsHitTestVisible && !queue.IsEnabled && queue.Visibility == Visibility.Visible &&
                Math.Abs(queue.ActualHeight - originalQueueHeight) < 1 &&
                Math.Abs(RelativeBounds(composer, _shell).Top - originalComposer.Top) < 1,
                "approval masks and disables queued rows without changing queue height or moving the composer");
            NativeUi.SetText(Prompt, "Synthetic queued Enter");
            Prompt.Focus(FocusState.Programmatic);
            DispatchShortcut(0x0D, control: false);
            await WaitAsync(() => ((IList)Call("QueueFor", _gateway.WorkChatId)!).Count == 3 && Prompt.Text.Length == 0,
                "native Enter still queues the draft while the approval is waiting");
            Check(ApprovalVisible && _gateway.ApprovalRequests.Length == 0,
                "Enter in the composer never approves the inline operation");
            prompts = ((IEnumerable)Call("QueueFor", _gateway.WorkChatId)!).Cast<QueuedPrompt>().ToArray();
            DispatchShortcut(0x1B, control: false);
            await SettleAsync();
            Check(ApprovalVisible && _gateway.ApprovalRequests.Length == 0,
                "Escape leaves the waiting inline approval undecided");
            NativeUi.SetText(Prompt, "A draft edited while awaiting approval");
            Check(Prompt.Text == "A draft edited while awaiting approval" && _gateway.ApprovalRequests.Length == 0,
                "the draft remains editable and merely displaying the card grants no permission");
            AppearanceService.ApplyPreferences(palette, "dark", fontSize, accent);
            await CheckGeometryAsync("dark-wide");
            NativeUi.Invoke(ApprovalButton("ApprovalDetailsToggle"));
            await SettleAsync();
            Check(NativeUi.IsVisible(NativeUi.ByName<TextBlock>(ApprovalCard, "ToolApprovalDetails")) &&
                NativeUi.ByName<TextBlock>(ApprovalCard, "ToolApprovalDetails").Text.Contains("https://bilibili.com/synthetic/path"),
                "expanding the inline details exposes the exact original URL without a second window");
            NativeUi.Invoke(ApprovalButton("ApprovalDetailsToggle"));

            await SearchAndSelectAsync(_gateway.StandaloneChatId, "Chat fixture");
            Check(!ApprovalVisible && _gateway.ApprovalRequests.Length == 0, "switching from A to B hides A's card without deciding it");
            object pendingB = CreatePending(_gateway.StandaloneChatId);
            var other = BrowserOperation("other-conversation-call", "other.example.invalid");
            Queue(pendingB, other);
            await WaitCardAsync("other.example.invalid");
            await SearchAndSelectAsync(_gateway.WorkChatId, "Work fixture");
            await WaitCardAsync("bilibili.com");
            Check(Prompt.Text == "A draft edited while awaiting approval", "returning to A restores its draft and pending approval identity");
            _gateway.HoldApprovals();
            NativeUi.Invoke(ApprovalButton("ApproveOnceButton"));
            await WaitAsync(() => _gateway.ApprovalRequests.Length == 1, "Approve once reaches the real approval HTTP client");
            Check(new[] { "ApproveAllButton", "ApproveOnceButton", "RejectApprovalButton" }.All(id => !ApprovalButton(id).IsEnabled),
                "an unacknowledged submission disables every decision action against duplicate clicks");
            Call("InlineApprovalDecisionRequested", ApprovalCard, ToolApprovalChoice.Reject);
            await SettleAsync();
            Check(_gateway.ApprovalRequests.Length == 1, "the production event handler also rejects a repeated decision while busy");
            _gateway.ReleaseApprovals();
            await WaitAsync(() => !ApprovalVisible, "acknowledged one-time approval retires only its card");
            CheckRequest(0, pendingA, first, true, "once");
            var queueAfter = ((IEnumerable)Call("QueueFor", _gateway.WorkChatId)!).Cast<QueuedPrompt>().ToArray();
            Check(queueAfter.SequenceEqual(prompts) && queueAfter.Select(prompt => prompt.Text)
                .SequenceEqual(new[] { "Synthetic queued first", "Synthetic queued second", "Synthetic queued Enter" }) &&
                queue.Opacity == 1 && queue.IsHitTestVisible && queue.IsEnabled,
                "acknowledgment restores the identical queued prompt objects, texts, order and interactions");

            var child = BrowserOperation("child-inline-call", "child.example.invalid");
            Queue(pendingA, child, childTask);
            await WaitCardAsync("child.example.invalid");
            _gateway.NextApprovalStatus = 503;
            NativeUi.Invoke(ApprovalButton("ApproveOnceButton"));
            await WaitAsync(() => _gateway.ApprovalRequests.Length == 2 && ApprovalButton("ApproveOnceButton").IsEnabled &&
                NativeUi.IsVisible(NativeUi.ByName<TextBlock>(ApprovalCard, "ToolApprovalError")),
                "a failed submission remains in its card with visible error and retry actions");
            CheckRequest(1, pendingA, child, true, "once");
            NativeUi.Invoke(ApprovalButton("ApproveAllButton"));
            await WaitAsync(() => _gateway.ApprovalRequests.Length == 3 && !ApprovalVisible, "task approval retries the original child decision");
            CheckRequest(2, pendingA, child, true, "task");
            Check(ShellField<LayoutState>("_layout").PermissionMode == permissionMode,
                "task-wide approval does not switch the saved composer permission mode");

            await SearchAndSelectAsync(_gateway.StandaloneChatId, "Chat fixture");
            await WaitCardAsync("other.example.invalid");
            AppearanceService.ApplyPreferences(palette, "light", fontSize, accent);
            ResizeInDips(480, 640);
            await WaitAsync(() => Element<Grid>("ShellGrid").ActualWidth < 800, "the owner reaches its actual narrow viewport");
            await CheckGeometryAsync("light-narrow");
            NativeUi.Invoke(ApprovalButton("ApprovalDetailsToggle"));
            await CheckGeometryAsync("light-narrow-details");
            NativeUi.Invoke(Element<Button>("ComposerExpandButton"));
            await CheckGeometryAsync("light-narrow-expanded-composer");
            NativeUi.Invoke(Element<Button>("ComposerExpandButton"));
            NativeUi.Invoke(ApprovalButton("ApprovalDetailsToggle"));
            NativeUi.Invoke(ApprovalButton("RejectApprovalButton"));
            await WaitAsync(() => _gateway.ApprovalRequests.Length == 4 && !ApprovalVisible, "explicit rejection retires B's own request");
            CheckRequest(3, pendingB, other, false, "once");
            var expired = BrowserOperation("expired-child-call", "expired.example.invalid");
            var following = BrowserOperation("following-child-call", "following.example.invalid");
            Queue(pendingB, expired, childTask);
            Queue(pendingB, following, childTask);
            await WaitCardAsync("expired.example.invalid");
            await SettleAsync();
            Check(NativeUi.Descendants<TextBlock>(ApprovalCard).Any(text => text.Text == "1/2" && NativeUi.IsVisible(text)),
                "multiple waiting calls share one card with an explicit queue count");
            Call("CloseTaskApproval", expired.ApprovalId!.Value, Guid.NewGuid());
            Check(ApprovalVisible, "a different task's close event cannot dismiss this child approval");
            Call("CloseTaskApproval", expired.ApprovalId!.Value, childTask);
            await WaitCardAsync("following.example.invalid");
            Call("CloseTaskApproval", following.ApprovalId!.Value, childTask);
            await WaitAsync(() => !ApprovalVisible, "owning task close events advance and dismiss expired requests without a decision");
            var timedOut = BrowserOperation("task-timeout-call", "timeout.example.invalid");
            Queue(pendingB, timedOut);
            await WaitCardAsync("timeout.example.invalid");
            _gateway.NextApprovalStatus = 408;
            _gateway.NextApprovalCode = "TOOL_RUN_TIMEOUT";
            NativeUi.Invoke(ApprovalButton("ApproveOnceButton"));
            await WaitAsync(() => _gateway.ApprovalRequests.Length == 5 && !ApprovalVisible,
                "the real gateway task timeout code retires an expired card without a retry loop");
            CheckRequest(4, pendingB, timedOut, true, "once");
            Queue(pendingB, BrowserOperation("cancelled-call", "cancelled.example.invalid"));
            await WaitCardAsync("cancelled.example.invalid");
            ((CancellationTokenSource)pendingB.GetType().GetProperty("Cancellation")!.GetValue(pendingB)!).Cancel();
            await WaitAsync(() => !ApprovalVisible, "reply cancellation dismisses its pending card without authorization");
            Check(_gateway.ApprovalRequests.Length == 5, "task closure and cancellation never create hidden approval or denial requests");
            File.WriteAllText(Path.Combine(_directory, "shell-inline-approval-geometry.json"),
                JsonSerializer.Serialize(geometry, new JsonSerializerOptions { WriteIndented = true }));
            File.AppendAllText(ResultPath, "Inline evidence: production queue/selection/card handlers and AgentApiClient → isolated HTTP approvals; no real model/tool execution.\n");
        }
        finally
        {
            _gateway.ReleaseApprovals();
            foreach (var (conversationId, pending) in owned)
            {
                Call("CloseReplyApprovals", pending);
                pendingReplies.Remove(conversationId);
                ((CancellationTokenSource)pending.GetType().GetProperty("Cancellation")!.GetValue(pending)!).Dispose();
            }
            ((IList)Call("QueueFor", _gateway.WorkChatId)!).Clear();
            Call("RefreshPromptQueue");
            Call("UpdateSendButtonState");
            AppearanceService.ApplyPreferences(palette, mode, fontSize, accent);
            ResizeInDips(1440, 900);
        }
    }
}
