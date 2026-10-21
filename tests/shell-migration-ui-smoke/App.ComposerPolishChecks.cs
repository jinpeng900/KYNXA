using System.Collections;
using System.Reflection;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.Views;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using PathShape = Microsoft.UI.Xaml.Shapes.Path;

namespace KYNXA_Desktop;

public partial class App
{
    private static SolidColorBrush? PresentedBackground(Button button) => NativeUi.Descendants<ContentPresenter>(button)
        .FirstOrDefault(presenter => presenter.Name == "ContentPresenter")?.Background as SolidColorBrush;

    private bool SendArrowUsesBrush(string key) => Element<PathShape>("SendArrow").Stroke is SolidColorBrush stroke &&
        ReferenceEquals(stroke, AppearanceService.GetBrush(key));

    private async Task CheckEmptyComposerPolishAsync()
    {
        var send = Element<Button>("SendButton");
        await SettleAsync();
        var background = PresentedBackground(send);
        Check(!send.IsEnabled && background is not null && background.Color == AppearanceService.ParseColor(AppearanceService.Current.Disabled),
            "the real disabled Send visual uses the current palette's readable disabled background");
        Check(SendArrowUsesBrush("KynxaDisabledTextBrush"),
            "the empty composer uses the shared disabled-text stroke so theme and high contrast updates reach its arrow");
        var arrow = Element<PathShape>("SendArrow");
        Check(arrow.Width == 20 && arrow.Height == 20 && Math.Abs(arrow.StrokeThickness - 5d / 3) < 0.000001 &&
            arrow.StrokeStartLineCap == PenLineCap.Round && arrow.StrokeEndLineCap == PenLineCap.Round &&
            arrow.StrokeLineJoin == PenLineJoin.Round,
            "the themed native arrow retains the original 20 DIP SVG layout, scaled line width and round caps");
        Check(!Element<Button>("ComposerAddButton").IsEnabled,
            "the unimplemented attachment action is explicitly unavailable");
        Check(Element<ComposerSurface>("ComposerHost").EditorHeight == 104,
            "a fresh layout starts with the compact default editor height");
        await CapturePresentedAsync("shell-polish-empty-wide.png");
    }

    private async Task CheckComposerStatesPolishAsync()
    {
        string originalDraft = Prompt.Text;
        var send = Element<Button>("SendButton");
        var pending = (IDictionary)typeof(ShellPage).GetField("_pendingReplies", PrivateInstance)!.GetValue(_shell)!;
        Type replyType = typeof(ShellPage).GetNestedType("PendingChatReply", BindingFlags.NonPublic)!;
        Guid conversationId = ActiveId ?? throw new InvalidOperationException("A real conversation is required for Send/Stop state checks.");
        object? ownedReply = null;
        try
        {
            NativeUi.SetText(Prompt, " \r\n ");
            await SettleAsync();
            Check(!send.IsEnabled && SendArrowUsesBrush("KynxaDisabledTextBrush"),
                "whitespace-only drafts retain the themed disabled stroke and cannot send");
            NativeUi.SetText(Prompt, "Synthetic nonempty draft");
            await SettleAsync();
            var activeBackground = PresentedBackground(send);
            Check(send.IsEnabled && activeBackground is not null &&
                new[] { AppearanceService.Current.Accent, AppearanceService.Current.AccentHover, AppearanceService.Current.AccentPressed }
                    .Select(AppearanceService.ParseColor).Contains(activeBackground.Color) &&
                SendArrowUsesBrush("KynxaOnAccentBrush"),
                "typing enables the real Send button in the current palette and uses its matching on-accent stroke");

            // Insert only an owned in-memory pending state. No stream, store, model or tool request is started.
            // 只加入夹具拥有的内存生成状态；不启动流、存储、模型或工具请求。
            ownedReply = Activator.CreateInstance(replyType, [conversationId, Guid.NewGuid(), "Synthetic pending state", null, null, "ask"])!;
            pending.Add(conversationId, ownedReply);
            NativeUi.SetText(Prompt, string.Empty);
            Call("UpdateSendButtonState");
            await SettleAsync();
            var stop = Element<FontIcon>("StopReplyIcon");
            Check(send.IsEnabled && NativeUi.IsVisible(stop) && Element<PathShape>("SendArrow").Visibility == Visibility.Collapsed &&
                ReferenceEquals(stop.Foreground, AppearanceService.GetBrush("KynxaOnAccentBrush")),
                "a pending reply keeps Stop enabled with the matching on-accent glyph even when the composer is empty");
            await CapturePresentedAsync("shell-polish-stop-wide.png");
            var ownedMessage = (KYNXA_Desktop.Models.UI.ChatMessageState)replyType.GetProperty("Message")!.GetValue(ownedReply)!;
            replyType.GetProperty("Presentation")!.SetValue(ownedReply,
                new KYNXA_Desktop.ViewModels.ConversationMessageViewModel(conversationId, ownedMessage));
            DateTimeOffset beforeStop = DateTimeOffset.UtcNow;
            NativeUi.Invoke(send);
            Check(((CancellationTokenSource)replyType.GetProperty("Cancellation")!.GetValue(ownedReply)!).IsCancellationRequested &&
                ownedMessage.Status == "streaming", "the real Stop button requests cancellation before treating a pending reply as terminal");
            Call("CancelPendingReply", conversationId);
            await SettleAsync();
            var recordedEnd = KYNXA_Desktop.Services.MessageTimePresentation.GetEnd(ownedMessage);
            Check(ownedMessage.Status == "interrupted" && recordedEnd >= beforeStop && recordedEnd <= DateTimeOffset.UtcNow &&
                !pending.Contains(conversationId) && _gateway.Writes == 0,
                "the production cancellation cleanup records a local terminal time for its owned message without writing gateway data");
        }
        finally
        {
            if (ownedReply is not null)
            {
                pending.Remove(conversationId);
                ((CancellationTokenSource)replyType.GetProperty("Cancellation")!.GetValue(ownedReply)!).Dispose();
            }
            NativeUi.SetText(Prompt, originalDraft);
            Call("UpdateSendButtonState");
        }
        Check(ActiveId == conversationId && Prompt.Text == originalDraft && _gateway.Writes == 0,
            "Send/Stop visual checks restore the exact conversation draft without writing formal data");
        ResizeInDips(620, 720);
        NativeUi.SetText(Prompt, string.Empty);
        await SettleAsync();
        try
        {
            var composer = Element<ComposerSurface>("ComposerHost");
            var sendBounds = send.TransformToVisual(composer).TransformBounds(new Windows.Foundation.Rect(0, 0, send.ActualWidth, send.ActualHeight));
            var model = Element<Button>("ModelPickerButton");
            var modelBounds = model.TransformToVisual(composer).TransformBounds(new Windows.Foundation.Rect(0, 0, model.ActualWidth, model.ActualHeight));
            Check(sendBounds.Right <= composer.ActualWidth + 1 && sendBounds.Bottom <= composer.ActualHeight + 1 && modelBounds.Right <= sendBounds.Left + 1,
                "the narrow composer keeps model selection and Send within its boundary without overlap");
            UiText.Initialize("en");
            await SettleAsync();
            await CapturePresentedAsync("shell-polish-empty-narrow-en.png");
            Check(!send.IsEnabled && Prompt.Text == string.Empty && ActiveId == conversationId,
                "English switching keeps the empty composer disabled and the conversation identity intact");
        }
        finally
        {
            UiText.Initialize("zh-CN");
            NativeUi.SetText(Prompt, originalDraft);
            ResizeInDips(1440, 900);
            await SettleAsync();
        }
    }
}
