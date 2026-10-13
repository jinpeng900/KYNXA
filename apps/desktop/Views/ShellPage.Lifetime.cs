using System.Diagnostics;
using KYNXA_Desktop.Services;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private static readonly TimeSpan CloseSaveTimeout = TimeSpan.FromSeconds(10);
    private bool _savingOnClose;
    private bool _closeApproved;
    private string? _closeError;

    private async void SaveBeforeClosing(AppWindow sender, AppWindowClosingEventArgs e)
    {
        if (_closeApproved) return;
        e.Cancel = true;
        if (_savingOnClose) return;
        if (StoragePaths.IsMigrating)
        {
            ProjectNotice.Message = UiText.Get("数据正在迁移，请完成迁移后再退出。");
            ProjectNotice.IsOpen = true;
            return;
        }
        _savingOnClose = true;
        _closeError = null;
        IsEnabled = false;
        var hiddenWindows = new List<Window>();
        try
        {
            // Hide immediately, but keep the window alive until its draft transaction settles.
            // 立即隐藏窗口，草稿事务结束前保留窗口生命周期；不强杀进程或停止共享网关。
            sender.Hide();
            foreach (var window in new Window?[] { _modelManagementWindow, _memoryManagementWindow,
                _toolManagementWindow, _retrievalSettingsWindow, _storageSettingsWindow })
                if (window?.AppWindow.IsVisible == true)
                {
                    hiddenWindows.Add(window);
                    window.AppWindow.Hide();
                }
            HideCloseDialogs();
            PauseRepliesForClose();
            using var deadline = new CancellationTokenSource(CloseSaveTimeout);
            while (_projectActionPending || _sendingPrompt || _retryingReplies.Count > 0)
                await Task.Delay(40, deadline.Token);
            if (StoragePaths.IsMigrating) throw new InvalidOperationException(UiText.Get("数据正在迁移，请完成迁移后再退出。"));
            if (_closeError is { } operationError) throw new InvalidOperationException(operationError);
            CaptureProjectDraft();
            CaptureStandaloneDraft();
            await _projectStore.SaveCatalogAsync(_projects, _standaloneChats, deadline.Token);
            // Receipt-time files are disposable presentation caches; they cannot hold exit indefinitely.
            // 接收时间文件是可丢弃的展示缓存，不能无限阻挡退出；正式会话保存已完成。
            var cacheFlush = MessageTimePresentation.FlushAsync();
            try { await cacheFlush.WaitAsync(TimeSpan.FromSeconds(2)); }
            catch (TimeoutException) { _ = ObserveLateCacheFlushAsync(cacheFlush); }
            catch (Exception error) { Debug.WriteLine($"Exit presentation-cache flush: {error.GetType().Name}"); }
            if (StoragePaths.IsMigrating) throw new InvalidOperationException(UiText.Get("数据正在迁移，请完成迁移后再退出。"));
            _closeApproved = true;
            App.Window.Close();
        }
        catch (Exception error)
        {
            sender.Show();
            foreach (var window in hiddenWindows)
            {
                try { window.AppWindow.Show(); }
                catch (System.Runtime.InteropServices.COMException) { /* Closed meanwhile. 中文：等待期间窗口可能已关闭。 */ }
            }
            App.Window.Activate();
            IsEnabled = !StoragePaths.IsMigrating;
            if (StoragePaths.IsMigrating)
            {
                ProjectNotice.Message = UiText.Get("数据正在迁移，请完成迁移后再退出。");
                ProjectNotice.IsOpen = true;
                return;
            }
            IsEnabled = true;
            string message = error is OperationCanceledException
                ? UiText.Get("退出保存超时，当前草稿仍保留在窗口中。请稍后重试。") : error.Message;
            // Existing pending dialogs may still be completing; avoid opening a second modal on their root.
            // 原有弹窗可能仍在结束，不能在同一根节点同时开启第二个模态框。
            if (HasPresentationModal())
            {
                ProjectNotice.Message = message;
                ProjectNotice.IsOpen = true;
            }
            else
            {
                try
                {
                    var choice = await new ContentDialog
                    {
                        XamlRoot = XamlRoot, Title = UiText.Get("草稿保存未完成"),
                        Content = message + UiText.Get(" 已提交的聊天由会话服务保存，尚未保存的草稿或排序可能丢失。"),
                        PrimaryButtonText = UiText.Get("仍然关闭"), CloseButtonText = UiText.Get("返回"), DefaultButton = ContentDialogButton.Close
                    }.ShowAsync();
                    if (choice == ContentDialogResult.Primary) { _closeApproved = true; App.Window.Close(); }
                }
                catch (Exception dialogError) when (dialogError is System.Runtime.InteropServices.COMException or InvalidOperationException)
                {
                    ProjectNotice.Message = message;
                    ProjectNotice.IsOpen = true;
                }
            }
        }
        finally
        {
            _savingOnClose = false;
            if (!_closeApproved)
            {
                _chatClosing = false;
                IsEnabled = !StoragePaths.IsMigrating;
                UpdateSendButtonState();
                if (!StoragePaths.IsMigrating) UpdateConversationPresentation(openAtBottom: false);
            }
        }
    }

    private static async Task ObserveLateCacheFlushAsync(Task flush)
    {
        try { await flush.ConfigureAwait(false); }
        catch (Exception error) { Debug.WriteLine($"Late exit presentation-cache flush: {error.GetType().Name}"); }
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
