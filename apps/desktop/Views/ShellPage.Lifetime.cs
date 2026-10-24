using System.Diagnostics;
using KYNXA_Desktop.Services;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private bool _savingOnClose;
    private bool _closeApproved;

    private async void MainWindow_Closing(AppWindow sender, AppWindowClosingEventArgs e)
    {
        if (_closeApproved) return;
        e.Cancel = true;
        if (_savingOnClose) return;
        _savingOnClose = true;
        _chatClosing = true;
        IsEnabled = false;
        try
        {
            // Exit hides immediately; only durable pause acknowledgement precedes stream disposal.
            // 退出立即隐藏窗口；仅在断开观察前提交持久暂停，不等待模型结束或目录保存。
            sender.Hide();
            foreach (var window in new Window?[] { _modelManagementWindow, _memoryManagementWindow,
                _toolManagementWindow, _retrievalSettingsWindow, _storageSettingsWindow })
                if (window?.AppWindow.IsVisible == true)
                    window.AppWindow.Hide();
            HideCloseDialogs();
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(3));
            Task pause = PauseActiveRepliesBeforeCloseAsync(deadline.Token);
            CaptureProjectDraft();
            CaptureStandaloneDraft();
            _ = SaveExitDraftsAsync();
            _ = ObserveExitWorkAsync(MessageTimePresentation.FlushAsync(), "presentation-cache flush");
            await pause;
        }
        catch (Exception error)
        {
            Debug.WriteLine($"Exit preparation: {error.GetType().Name}");
        }
        finally
        {
            // The shared gateway retains checkpoints and settles dispatched receipts after pause.
            // 共享网关保留检查点并结算已派发回执；草稿失败不撤销退出，也不重放原请求。
            try { PauseRepliesForClose(); }
            catch (Exception error) { Debug.WriteLine($"Exit stream disposal: {error.GetType().Name}"); }
            try { _ = ObserveExitWorkAsync(Task.WhenAll(WorkspaceTerminal.CloseAllAsync(), DocumentPanel.CloseAllAsync()), "workspace cleanup"); }
            catch (Exception error) { Debug.WriteLine($"Exit workspace cleanup: {error.GetType().Name}"); }
            _closeApproved = true;
            App.Window.Close();
        }
    }

    private Task PauseActiveRepliesBeforeCloseAsync(CancellationToken cancellationToken) =>
        Task.WhenAll(_pendingReplies.Values.ToArray().Select(pending => PauseReplyBeforeCloseAsync(pending, cancellationToken)));

    private async Task PauseReplyBeforeCloseAsync(PendingChatReply pending, CancellationToken cancellationToken)
    {
        pending.StopRequested = true;
        try
        {
            // Stable request identity also covers acceptance before the first started event.
            // 稳定请求身份覆盖首个 started 事件前的受理窗口；恢复观察同时携带原任务身份。
            await _modelApiClient.PauseReplyAsync(pending.ConversationId, pending.StreamRequestId, pending.TaskId, cancellationToken);
            pending.PauseConfirmed = true;
        }
        catch (Exception error) { Debug.WriteLine($"Exit task pause: {error.GetType().Name}"); }
    }

    private async Task SaveExitDraftsAsync()
    {
        // Never queue exit behind a catalog mutation or write to a root being migrated.
        // 退出不排在目录变更后等待，也不向迁移中的根目录写草稿；正式聊天不由此保存。
        if (!_projectsReady || _projectActionPending || StoragePaths.IsMigrating) return;
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(1));
        try { await _projectStore.SaveCatalogAsync(_projects, _standaloneChats, deadline.Token); }
        catch (Exception error) { Debug.WriteLine($"Exit draft save: {error.GetType().Name}"); }
    }

    private static async Task ObserveExitWorkAsync(Task operation, string name)
    {
        try { await operation.ConfigureAwait(false); }
        catch (Exception error) { Debug.WriteLine($"Exit {name}: {error.GetType().Name}"); }
    }

    private void HideCloseDialogs()
    {
        if (XamlRoot is null) return;
        static void HideDialog(DependencyObject? element)
        {
            if (element is ContentDialog dialog) { dialog.Hide(); return; }
            if (element is null) return;
            for (int index = 0; index < VisualTreeHelper.GetChildrenCount(element); index++)
                HideDialog(VisualTreeHelper.GetChild(element, index));
        }
        foreach (var popup in VisualTreeHelper.GetOpenPopupsForXamlRoot(XamlRoot))
            if (popup.IsOpen) HideDialog(popup.Child);
    }
}
