using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private bool _toolResultOpen;
    private CancellationTokenSource? _toolResultCancellation;
    private TaskCompletionSource? _toolResultClosed;

    private async void ToolResultRequested(object? sender, ToolResultRequest request)
    {
        if (_toolResultOpen || _chatClosing || ActiveChatId != request.ConversationId || XamlRoot is null ||
            sender is not (ConversationTranscript or ConversationScreenshotsPanel) || request.Tool.ResultRef is null) return;
        var currentTool = ActiveMessages.FirstOrDefault(row => row.Message.Id == request.MessageId)?.ToolActivities
            .FirstOrDefault(tool => tool.ToolCallId == request.Tool.ToolCallId && tool.ResultRef == request.Tool.ResultRef);
        if (currentTool is null || (sender is ConversationScreenshotsPanel &&
            (!ConversationScreenshotSources.IsScreenshotTool(currentTool.Name) || currentTool.Status != "completed"))) return;
        _toolResultOpen = true;
        using var cancellation = new CancellationTokenSource();
        _toolResultCancellation = cancellation;
        var closed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        _toolResultClosed = closed;
        void Cancel(object? changed, EventArgs e) => cancellation.Cancel();
        ConversationMessages.ConversationChanged += Cancel;
        try
        {
            if (ConversationScreenshotSources.IsScreenshotTool(currentTool.Name) && currentTool.Status == "completed")
            {
                using var viewer = new ScreenshotViewerWindow(_agentApiClient,
                    new(request.ConversationId, request.MessageId, currentTool), cancellation.Token);
                await viewer.ShowAsync();
            }
            else
            {
                using var viewer = new ToolResultDialog(XamlRoot, _agentApiClient, request.ConversationId, currentTool, cancellation.Token);
                await viewer.ShowAsync();
            }
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested) { }
        catch (InvalidOperationException) { }
        finally
        {
            ConversationMessages.ConversationChanged -= Cancel; _toolResultOpen = false;
            _toolResultCancellation = null; _toolResultClosed = null; closed.TrySetResult();
        }
    }
}
