using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using KYNXA.Contracts;

namespace KYNXA_Desktop
{
    public static class App
    {
        public static Window Window { get; set; } = null!;
    }
}

namespace KYNXA_Desktop.Services
{
    public static class StoragePaths
    {
        public static bool IsMigrating { get; set; }
    }

    public static class UiText
    {
        public static string Get(string text) => text;
    }

    // This minimal fixture exercises close/save decisions with SDK controls, without the unrelated application palette.
    // 此最小夹具用 SDK 原生控件核验退出与保存决定，不加载无关的应用配色资源。
    public static class DialogPresentation
    {
        public static Windows.Foundation.IAsyncOperation<ContentDialogResult> ShowAsync(ContentDialog dialog) => dialog.ShowAsync();
    }

    public static class MessageTimePresentation
    {
        public static int FlushCalls { get; private set; }

        public static Task FlushAsync()
        {
            FlushCalls++;
            return Task.CompletedTask;
        }
    }

    // Observe the production owner's async cleanup ordering without starting terminal or browser processes.
    // 不启动终端或浏览器进程，仅观察生产所有者异步清理的调用与完成顺序。
    public sealed class FakeWorkspaceCleanup
    {
        public int CloseCalls { get; private set; }
        public TaskCompletionSource? CloseCompletion { get; set; }

        public Task CloseAllAsync()
        {
            CloseCalls++;
            return CloseCompletion?.Task ?? Task.CompletedTask;
        }
    }

    // Record synthetic snapshots and cancellation without opening a gateway or a data store.
    // 记录虚构快照与取消状态，不启动网关，也不访问数据存储。
    public sealed class FakeProjectStore
    {
        private readonly TaskCompletionSource _saveCompletion = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public int SaveCalls { get; private set; }
        public string[] SavedProjects { get; private set; } = [];
        public string[] SavedChats { get; private set; } = [];
        public CancellationToken SaveToken { get; private set; }
        public Exception? SaveFailure { get; set; }

        public async Task SaveCatalogAsync(IReadOnlyList<string> projects, IEnumerable<string> chats,
            CancellationToken cancellationToken = default)
        {
            SaveCalls++;
            SavedProjects = projects.ToArray();
            SavedChats = chats.ToArray();
            SaveToken = cancellationToken;
            if (SaveFailure is { } failure) throw failure;
            await _saveCompletion.Task.WaitAsync(cancellationToken);
        }

        public void CompleteSave() => _saveCompletion.TrySetResult();
    }
}

namespace KYNXA_Desktop.Views
{
    // Link the production close handler while replacing its unrelated shell and persistence dependencies.
    // 直接链接生产关闭处理器，仅替换无关界面与持久化依赖。
    public sealed partial class ShellPage : Page
    {
        // No live replies exist in this fixture; unexpected gateway pause calls fail without network I/O.
        // 此夹具没有运行中的回复；意外暂停调用直接失败，不访问真实网关。
        private sealed class PendingChatReply
        {
            public Guid ConversationId { get; init; }
            public Guid? TaskId { get; init; }
            public bool StopRequested { get; set; }
            public bool StopControlPending { get; set; }
            public bool PauseConfirmed { get; set; }
            public TaskCompletionSource<Guid?> TaskRegistered { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
            public TaskCompletionSource Finished { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
        }

        private sealed record FixtureProject(Guid Id);
        private sealed class UnexpectedTaskApi
        {
            public Task<TaskDetailResponse> GetAsync(Guid conversationId, Guid taskId, Guid? projectId, CancellationToken token) =>
                Task.FromException<TaskDetailResponse>(new InvalidOperationException("Unexpected live task lookup in window-close fixture."));
            public Task<TaskControlResponse> ControlAsync(Guid conversationId, Guid taskId, ulong version, string action,
                Guid? projectId, CancellationToken token) =>
                Task.FromException<TaskControlResponse>(new InvalidOperationException("Unexpected live task control in window-close fixture."));
        }

        private readonly Dictionary<Guid, PendingChatReply> _pendingReplies = [];
        private readonly UnexpectedTaskApi _tasksApiClient = new();
        private static (object? Chat, FixtureProject? Project) FindChat(Guid conversationId) => (null, null);
        private readonly Services.FakeProjectStore _projectStore;
        private readonly List<string> _projects = ["synthetic-project"];
        private readonly List<string> _standaloneChats = ["synthetic-chat"];
        private readonly HashSet<Guid> _retryingReplies = [];
        private bool _projectActionPending;
        private bool _sendingPrompt = false;
        private bool _chatClosing;
        private Window? _modelManagementWindow;
        private Window? _memoryManagementWindow = null;
        private Window? _toolManagementWindow = null;
        private Window? _retrievalSettingsWindow = null;
        private Window? _storageSettingsWindow = null;

        public InfoBar ProjectNotice { get; } = new();
        public Services.FakeWorkspaceCleanup WorkspaceTerminal { get; } = new();
        public Services.FakeWorkspaceCleanup DocumentPanel { get; } = new();
        public TextBox DraftEditor { get; } = new() { Text = "unsaved synthetic draft / 临时草稿", AcceptsReturn = true };
        public Services.FakeProjectStore Store => _projectStore;
        public int PauseCalls { get; private set; }
        public int ProjectCaptures { get; private set; }
        public int StandaloneCaptures { get; private set; }
        public int SendRefreshes { get; private set; }
        public int ConversationRefreshes { get; private set; }
        public string? CapturedDraft { get; private set; }
        public bool IsClosing => _savingOnClose;
        public bool IsChatClosing => _chatClosing;

        public ShellPage(Services.FakeProjectStore store)
        {
            _projectStore = store;
            var content = new StackPanel { Margin = new Thickness(20), Spacing = 12 };
            content.Children.Add(new TextBlock { Text = "Isolated production window-close handler" });
            content.Children.Add(DraftEditor);
            content.Children.Add(ProjectNotice);
            Content = content;
        }

        public void AttachOwner(Window owner)
        {
            owner.AppWindow.Closing += SaveBeforeClosing;
        }

        public void SetMetadataBusy(bool busy) => _projectActionPending = busy;
        public void AttachSecondary(Window window)
        {
            _modelManagementWindow = window;
            window.Closed += (_, _) =>
            {
                if (ReferenceEquals(_modelManagementWindow, window)) _modelManagementWindow = null;
            };
        }

        public async Task<ContentDialogResult> ShowPendingMetadataDialogAsync()
        {
            _projectActionPending = true;
            try
            {
                return await new ContentDialog
                {
                    XamlRoot = XamlRoot, Title = "Pending synthetic metadata action",
                    Content = "Closing must cancel this prompt before it waits for the action.",
                    PrimaryButtonText = "Save", CloseButtonText = "Cancel"
                }.ShowAsync();
            }
            finally { _projectActionPending = false; }
        }

        private void PauseRepliesForClose()
        {
            PauseCalls++;
            _chatClosing = true;
        }

        private void CaptureProjectDraft()
        {
            ProjectCaptures++;
            CapturedDraft = DraftEditor.Text;
        }

        private void CaptureStandaloneDraft()
        {
            StandaloneCaptures++;
            CapturedDraft = DraftEditor.Text;
        }

        private void UpdateSendButtonState() => SendRefreshes++;
        private void UpdateConversationPresentation(bool openAtBottom = true) => ConversationRefreshes++;
        private bool HasPresentationModal() => MemoryUiSmoke.NativeUi.OpenDialog(this) is not null;

        private async Task ShowProjectErrorAsync(string title, string message) => await new ContentDialog
        {
            XamlRoot = XamlRoot, Title = title, Content = message, CloseButtonText = "知道了"
        }.ShowAsync();
    }
}
