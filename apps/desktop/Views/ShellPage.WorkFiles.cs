using System.Collections.ObjectModel;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    public ObservableCollection<WorkspaceFileEntry> WorkFiles { get; } = [];
    private string? _workFilesContext;
    private string? _workFilesRoot;
    private string _workFilesListStatus = "点击“刷新文件”读取关联目录，不会上传文件。";
    private int _workFilesVersion;
    private int _workFilesOperation;
    private CancellationTokenSource? _workFilesCancellation;

    // Called when the current work changes. It intentionally does not touch the filesystem.
    private void SynchronizeWorkFilesContext()
    {
        if (_chatClosing) return;
        var chat = ViewModel.IsChatMode ? null : _activeProjectChat;
        var project = chat is null ? null : _projects.FirstOrDefault(candidate => candidate.Chats.Contains(chat));
        string? root = project?.FolderPath;
        string context = $"{chat?.Id}|{root}";
        if (context == _workFilesContext) return;
        _workFilesContext = context;
        _workFilesRoot = root;
        _workFilesVersion++;
        CancelWorkFilesRead();
        WorkFiles.Clear();
        _workFilesListStatus = chat is null ? "先选择一个工作。"
            : string.IsNullOrWhiteSpace(root) ? "此工作没有关联文件夹，请先关联本地目录。"
            : "点击“刷新文件”读取关联目录，不会上传文件。";
        if (WorkFilesStatus is not null) WorkFilesStatus.Text = _workFilesListStatus;
        if (RefreshWorkFilesButton is not null) RefreshWorkFilesButton.IsEnabled = chat is not null && !string.IsNullOrWhiteSpace(root);
    }

    private void CancelWorkFilesRead()
    {
        _workFilesOperation++;
        _workFilesCancellation?.Cancel();
        _workFilesCancellation?.Dispose();
        _workFilesCancellation = null;
    }

    private bool WorkFilesReadIsCurrent(int version, int operation)
    {
        if (_chatClosing) return false;
        SynchronizeWorkFilesContext();
        return version == _workFilesVersion && operation == _workFilesOperation;
    }

    private async void RefreshWorkFiles_Click(object sender, RoutedEventArgs e)
    {
        SynchronizeWorkFilesContext();
        if (string.IsNullOrWhiteSpace(_workFilesRoot) || _chatClosing) return;
        CancelWorkFilesRead();
        var cancellation = _workFilesCancellation = new CancellationTokenSource();
        int version = _workFilesVersion;
        int operation = _workFilesOperation;
        string root = _workFilesRoot;
        WorkFilesStatus.Text = "正在读取目录…";
        RefreshWorkFilesButton.IsEnabled = false;
        try
        {
            var result = await WorkspaceFileReader.ListAsync(root, cancellation.Token);
            if (!WorkFilesReadIsCurrent(version, operation)) return;
            WorkFiles.Clear();
            foreach (var entry in result.Entries) WorkFiles.Add(entry);
            _workFilesListStatus = result.Message;
            WorkFilesStatus.Text = result.Message;
        }
        catch (OperationCanceledException) { }
        finally
        {
            if (!_chatClosing && WorkFilesReadIsCurrent(version, operation))
            {
                RefreshWorkFilesButton.IsEnabled = true;
            }
            if (ReferenceEquals(_workFilesCancellation, cancellation)) _workFilesCancellation = null;
            cancellation.Dispose();
        }
    }

    private async void WorkFilesList_ItemClick(object sender, ItemClickEventArgs e)
    {
        SynchronizeWorkFilesContext();
        if (e.ClickedItem is not WorkspaceFileEntry entry || !WorkFiles.Contains(entry) || _featureInfoOpen || _projectActionPending || _chatClosing) return;
        if (entry.IsDirectory)
        {
            await ShowFeatureInfoAsync("文件夹", "本轮只显示关联目录的一级内容，不展开子目录。");
            return;
        }
        CancelWorkFilesRead();
        var cancellation = _workFilesCancellation = new CancellationTokenSource();
        int version = _workFilesVersion;
        int operation = _workFilesOperation;
        string? root = _workFilesRoot;
        WorkFilesStatus.Text = "正在读取文件…";
        RefreshWorkFilesButton.IsEnabled = false;
        try
        {
            var result = await WorkspaceFileReader.ReadTextAsync(root, entry.FullPath, cancellation.Token);
            if (!WorkFilesReadIsCurrent(version, operation)) return;
            WorkFilesStatus.Text = _workFilesListStatus;
            if (!result.Success) { await ShowFeatureInfoAsync("无法预览 " + entry.Name, result.Message); return; }
            if (_featureInfoOpen || XamlRoot is null) return;
            _featureInfoOpen = true;
            try
            {
                double width = Math.Max(180, Math.Min(460, XamlRoot.Size.Width - 100));
                double height = Math.Max(100, Math.Min(420, XamlRoot.Size.Height - 240));
                var text = new TextBox
                {
                    IsReadOnly = true, AcceptsReturn = true, TextWrapping = TextWrapping.NoWrap,
                    FontFamily = new FontFamily("Consolas"), FontSize = 12, Height = height
                };
                ScrollViewer.SetHorizontalScrollBarVisibility(text, ScrollBarVisibility.Auto);
                ScrollViewer.SetVerticalScrollBarVisibility(text, ScrollBarVisibility.Auto);
                // Configure multiline editing before assigning the complete file text.
                text.Text = result.Text.Length == 0 ? "（空文件）" : result.Text;
                var content = new StackPanel { Width = width, Spacing = 12 };
                content.Children.Add(new TextBlock { Text = result.Message, TextWrapping = TextWrapping.Wrap, FontSize = 12 });
                content.Children.Add(text);
                var title = new TextBlock
                {
                    Text = entry.Name, TextWrapping = TextWrapping.Wrap, MaxLines = 2,
                    TextTrimming = TextTrimming.CharacterEllipsis, MaxWidth = width
                };
                ToolTipService.SetToolTip(title, entry.Name);
                await new ContentDialog
                {
                    XamlRoot = XamlRoot, Title = title, Content = content,
                    CloseButtonText = "关闭", DefaultButton = ContentDialogButton.Close
                }.ShowAsync();
            }
            finally { _featureInfoOpen = false; }
        }
        catch (OperationCanceledException) { }
        catch (Exception error) when (error is System.Runtime.InteropServices.COMException or InvalidOperationException)
        {
            if (WorkFilesReadIsCurrent(version, operation)) WorkFilesStatus.Text = "预览暂时无法打开，请关闭其他对话框后重试。";
        }
        finally
        {
            if (!_chatClosing && WorkFilesReadIsCurrent(version, operation))
            {
                RefreshWorkFilesButton.IsEnabled = !string.IsNullOrWhiteSpace(_workFilesRoot);
            }
            if (ReferenceEquals(_workFilesCancellation, cancellation)) _workFilesCancellation = null;
            cancellation.Dispose();
        }
    }
}
