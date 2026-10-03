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
            sender is not ConversationTranscript transcript || request.Tool.ResultRef is null) return;
        _toolResultOpen = true;
        using var cancellation = new CancellationTokenSource();
        _toolResultCancellation = cancellation;
        var closed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        _toolResultClosed = closed;
        void Cancel(object? changed, EventArgs e) => cancellation.Cancel();
        transcript.ConversationChanged += Cancel;
        try
        {
            using var viewer = new ToolResultDialog(XamlRoot, _agentApiClient, request.ConversationId, request.Tool, cancellation.Token);
            await viewer.ShowAsync();
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested) { }
        catch (InvalidOperationException) { }
        finally
        {
            transcript.ConversationChanged -= Cancel; _toolResultOpen = false;
            _toolResultCancellation = null; _toolResultClosed = null; closed.TrySetResult();
        }
    }
}
