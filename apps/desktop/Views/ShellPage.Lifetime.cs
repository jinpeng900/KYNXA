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
            // The local acknowledgement transfers shutdown ownership; the foreground does not save tasks itself.
            // 本机回执确认后台已接管退出；前台不再逐条暂停或等待任务保存。
            sender.Hide();
            foreach (var window in new Window?[] { _modelManagementWindow, _memoryManagementWindow,
                _toolManagementWindow, _retrievalSettingsWindow, _storageSettingsWindow })
                if (window?.AppWindow.IsVisible == true)
                    window.AppWindow.Hide();
            HideCloseDialogs();
            CaptureProjectDraft();
            CaptureStandaloneDraft();
            _ = SaveExitDraftsAsync();
            await ExpansionRemoteService.StopAsync();
            await ModelGatewayService.RequestShutdownAsync();
        }
        catch (Exception error)
        {
            Debug.WriteLine($"Exit preparation: {error.GetType().Name}");
        }
        finally
        {
            _closeApproved = true;
            try { App.Window.Close(); }
            finally { Application.Current.Exit(); }
        }
    }

    private async Task SaveExitDraftsAsync()
    {
        // Preserve the existing best-effort draft write without delaying exit behind catalog work or migration.
        // 保留原有的尽力保存草稿，不让退出排在目录操作或迁移后等待。
        if (!_projectsReady || _projectActionPending || StoragePaths.IsMigrating) return;
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(1));
        try { await _projectStore.SaveCatalogAsync(_projects, _standaloneChats, deadline.Token); }
        catch (Exception error) { Debug.WriteLine($"Exit draft save: {error.GetType().Name}"); }
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
