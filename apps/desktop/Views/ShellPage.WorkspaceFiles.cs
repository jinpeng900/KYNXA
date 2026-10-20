using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    /// <summary>Whether the file mirror currently owns the right pane's preview row.
    /// 文件镜像当前是否占用右栏预览行。</summary>
    private bool _workspaceFilesVisible;
    private Guid? _workspaceFilesConversation;

    /// <summary>The mirror is entered from the right-pane menu and returns there when dismissed.
    /// 文件镜像从右栏菜单进入，返回时回到该菜单。</summary>
    private void ShowWorkspaceFiles()
    {
        if (_chatClosing || ActiveChatId is null) return;
        _workspaceFilesVisible = true;
        _layout.PreviewVisible = true;
        ApplyLayout();
        SaveLayout();
    }

    private void CloseWorkspaceFiles()
    {
        if (!_workspaceFilesVisible) return;
        _workspaceFilesVisible = false;
        ApplyLayout();
        SaveLayout();
    }

    /// <summary>A visible mirror keeps polling; leaving the row for a tab stops it and releases the cancellation.
    /// 镜像可见时持续轮询；把预览行让给标签时停止轮询并释放取消源。</summary>
    private void SynchronizeWorkspaceFiles(bool filesOwnRow)
    {
        if (filesOwnRow && ActiveChatId is Guid conversation && _workspaceFilesConversation != conversation)
        {
            _workspaceFilesConversation = conversation;
            WorkspaceFiles.ShowConversation(conversation);
        }
        WorkspaceFiles.SetPreviewEnabled(filesOwnRow);
        WorkspaceFiles.Visibility = filesOwnRow ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>A mirrored file opens through the existing preview surface, which re-authorizes the exact path.
    /// 镜像中的文件通过既有预览面打开，并由该接口重新授权精确路径。</summary>
    private void OpenWorkspacePath(string path)
    {
        if (_chatClosing || ActiveChatId is not Guid conversation || string.IsNullOrWhiteSpace(path)) return;
        _workspaceFilesVisible = false;
        AddWorkspaceTab(conversation, ConversationWorkspaceSources.File(path), select: true);
    }

    /// <summary>A saved upload keeps using the attachment endpoints rather than a mirrored copy.
    /// 已保存上传继续走附件接口，不使用镜像副本。</summary>
    private void OpenWorkspaceAttachment(WorkspaceTreeAttachment attachment)
    {
        if (_chatClosing || ActiveChatId is not Guid conversation) return;
        _workspaceFilesVisible = false;
        AddWorkspaceTab(conversation, new(
            $"attachment/{conversation:D}/{attachment.MessageId:D}/{attachment.Id:D}", "document", attachment.Name,
            $"attachment:{attachment.MessageId:D}/{attachment.Id:D}"), select: true);
    }
}
