using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop.Controls
{
    // Only the event-owning transcript surface is substituted. The Shell handler and viewer are production files.
    public sealed class ConversationTranscript : Grid
    {
        public event EventHandler? ConversationChanged;
        public void NotifyConversationChanged() => ConversationChanged?.Invoke(this, EventArgs.Empty);
    }
}

namespace KYNXA_Desktop.Views
{
    public sealed partial class ShellPage : Grid
    {
        private readonly IAgentApi _agentApiClient;
        private bool _chatClosing = false;
        private Guid? ActiveChatId { get; set; }
        private IReadOnlyList<ConversationMessageViewModel> ActiveMessages { get; }
        private ConversationTranscript ConversationMessages { get; } = new();
        public ConversationScreenshotsPanel ScreenshotPanel { get; } = new();
        public Task? ResultClosed => _toolResultClosed?.Task;

        public ShellPage(IAgentApi api, Guid conversation, IReadOnlyList<ConversationMessageViewModel> messages)
        { _agentApiClient = api; ActiveChatId = conversation; ActiveMessages = messages; }
        public void OpenScreenshot(ToolResultRequest request) => ToolResultRequested(ScreenshotPanel, request);
        public void ChangeConversation(Guid conversation)
        { ActiveChatId = conversation; ConversationMessages.NotifyConversationChanged(); }
    }
}
