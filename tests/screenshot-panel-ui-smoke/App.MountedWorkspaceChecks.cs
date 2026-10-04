using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using KYNXA.Contracts;
using System.Text.Json;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Automation.Provider;
using Microsoft.UI.Xaml.Controls;

namespace ScreenshotPanelUiSmoke;

public partial class App
{
    private async Task CheckMountedScreenshotLayoutAsync()
    {
        using var header = new MountedWorkspaceHeader { Margin = new Thickness(10, 3, 40, 0) };
        using var screenshots = new ConversationScreenshotsPanel { Margin = new Thickness(14, 10, 14, 14) };
        var sidebar = new Grid();
        sidebar.RowDefinitions.Add(new() { Height = new GridLength(32) });
        sidebar.RowDefinitions.Add(new() { Height = new GridLength(1, GridUnitType.Star) });
        sidebar.Children.Add(header); Grid.SetRow(screenshots, 1); sidebar.Children.Add(screenshots);
        Grid.SetColumn(sidebar, 1); _layout.Children.Add(sidebar);
        var api = new FakeScreenshotApi(); screenshots.ConfigureApi(api);
        var conversation = Guid.NewGuid(); var tool = Screenshot("mounted-combined-screenshot");
        header.ShowProject(Guid.NewGuid(), Path.Combine(_directory, "Work", "KYNXA"));
        screenshots.ShowConversation(conversation, [Message(conversation, tool)]);
        screenshots.SetPreviewEnabled(true);
        try
        {
            await WaitAsync(() => api.Reads.Count == 1 && screenshots.IsLoaded, "combined sidebar lazily reads one PNG archive beneath the mounted folder");
            api.Reads[0].Completion.TrySetResult(new ToolResultResponse(JsonSerializer.SerializeToElement(
                new { content = new[] { new { type = "image", mimeType = "image/png", data = _png } } })));
            var image = NativeUi.ByName<Image>(screenshots, "ScreenshotPanelImage");
            await WaitAsync(() => image.Source is not null && image.ActualWidth > 0, "combined sidebar renders the archived PNG alongside the mounted directory");
            Check(header.HasMountedFolder && header.Visibility == Visibility.Visible && screenshots.Visibility == Visibility.Visible &&
                NativeUi.ByName<TextBlock>(header, "MountedWorkspaceName").Text == "KYNXA", "mounted short directory name and screenshot are visible together");
            double headerBottom = header.TransformToVisual(sidebar).TransformPoint(new Windows.Foundation.Point(0, header.ActualHeight)).Y;
            double screenshotTop = screenshots.TransformToVisual(sidebar).TransformPoint(new Windows.Foundation.Point()).Y;
            var imageOrigin = image.TransformToVisual(sidebar).TransformPoint(new Windows.Foundation.Point());
            Check(screenshotTop >= headerBottom && imageOrigin.X >= 0 && imageOrigin.X + image.ActualWidth <= sidebar.ActualWidth + 0.5 &&
                image.Stretch == Microsoft.UI.Xaml.Media.Stretch.Uniform, "screenshot sits below the mounted header and fits the sidebar without horizontal cropping");
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "mounted-screenshot-combined.png"));
            Check(api.Reads.Count == 1 && api.Reads[0].Conversation == conversation && api.Reads[0].Reference == tool.ResultRef,
                "combined layout preserves the single current-chat archive request");
        }
        finally { screenshots.Dispose(); header.Dispose(); _layout.Children.Remove(sidebar); }
    }

    private async Task CheckMountedWorkspaceAsync()
    {
        using var header = new MountedWorkspaceHeader { Margin = new Thickness(14, 8, 14, 0), VerticalAlignment = VerticalAlignment.Top };
        Grid.SetColumn(header, 1); _layout.Children.Add(header);
        Guid projectA = Guid.NewGuid(), projectB = Guid.NewGuid();
        string folderA = Path.Combine(_directory, "Work", "KYNXA"), folderB = Path.Combine(_directory, "Work", "中英文开发项目");
        var open = NativeUi.ByName<Button>(header, "MountedWorkspaceOpen");
        var more = NativeUi.ByName<Button>(header, "MountedWorkspaceMore");
        var menu = (MenuFlyout)more.Flyout;
        var change = (MenuFlyoutItem)menu.Items[0]; var unmount = (MenuFlyoutItem)menu.Items[1];
        var opened = new List<MountedWorkspaceRequest>(); var changed = new List<MountedWorkspaceRequest>(); var unmounted = new List<MountedWorkspaceRequest>();
        header.OpenRequested += (_, request) => opened.Add(request);
        header.ChangeRequested += (_, request) => changed.Add(request);
        header.UnmountRequested += (_, request) => unmounted.Add(request);

        async Task ShowMenuAsync()
        {
            NativeUi.Invoke(more);
            await WaitAsync(() => change.IsLoaded && unmount.IsLoaded, "mounted folder menu opens through the native button");
        }
        try
        {
            Check(!header.HasMountedFolder && header.Visibility == Visibility.Collapsed, "unmounted header starts hidden without creating a workspace");
            header.ShowProject(projectA, null);
            Check(!header.HasMountedFolder && header.Visibility == Visibility.Collapsed, "a project without a folder has no mounted header");
            header.ShowProject(Guid.Empty, folderA);
            Check(!header.HasMountedFolder, "empty project identity cannot display a mounted folder");
            header.ShowProject(projectA, "relative/work");
            Check(!header.HasMountedFolder, "relative model-like folder text is not treated as a mounted absolute path");
            header.ShowProject(projectA, folderA);
            await WaitAsync(() => header.IsLoaded && header.ActualWidth > 0, "production mounted header loads in the actual sidebar");
            var name = NativeUi.ByName<TextBlock>(header, "MountedWorkspaceName");
            Check(header.HasMountedFolder && name.Text == "KYNXA" && header.Visibility == Visibility.Visible,
                "mounted header shows only the directory basename");
            Check(ToolTipService.GetToolTip(open) as string == folderA && ToolTipService.GetToolTip(header) as string == folderA,
                "hover keeps the full formal folder path available");
            Check(NativeUi.Descendants<FontIcon>(header).Any(icon => icon.Glyph == "\uE8B7") &&
                NativeUi.Descendants<TextBlock>(header).All(text => text.Text != folderA), "folder icon and short name do not turn the header into a path or metadata card");
            NativeUi.Invoke(open); await SettleAsync();
            Check(opened.SequenceEqual([new MountedWorkspaceRequest(projectA, folderA)]), "open event carries the exact formal project and folder identity");
            Check(change.Text == "重新关联文件夹" && unmount.Text == "取消关联文件夹", "mounted folder menu exposes concise change and unlink actions in Chinese");
            await ShowMenuAsync(); InvokeMountedMenuItem(change); await SettleAsync();
            Check(changed.SequenceEqual([new MountedWorkspaceRequest(projectA, folderA)]), "change event is delivered through the actual native menu item");
            await ShowMenuAsync(); InvokeMountedMenuItem(unmount); await SettleAsync();
            Check(unmounted.SequenceEqual([new MountedWorkspaceRequest(projectA, folderA)]), "unlink event is delivered through the actual native menu item");
            Check(header.HasMountedFolder && name.Text == "KYNXA" && !Directory.Exists(folderA) && _api.Reads.Count == 0,
                "the display control emits requests without storing metadata, creating folders, or fetching model data");
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "mounted-header-zh.png"));

            await ShowMenuAsync();
            int changesBeforeSwitch = changed.Count;
            header.ShowProject(projectB, folderB); await SettleAsync();
            InvokeMountedMenuItem(change); await SettleAsync();
            Check(changed.Count == changesBeforeSwitch && name.Text == "中英文开发项目", "switching projects invalidates the old menu request instead of acting on either project");
            await ShowMenuAsync();
            string movedFolder = Path.Combine(_directory, "Moved", "中英文开发项目");
            header.ShowProject(projectB, movedFolder); await SettleAsync(); InvokeMountedMenuItem(unmount); await SettleAsync();
            Check(unmounted.Count == 1 && ToolTipService.GetToolTip(open) as string == movedFolder,
                "changing a folder within the same project invalidates the prior menu's old path");
            await ShowMenuAsync(); header.ShowProject(projectB, movedFolder); InvokeMountedMenuItem(change); await SettleAsync();
            Check(changed.Count == changesBeforeSwitch + 1 && changed[^1] == new MountedWorkspaceRequest(projectB, movedFolder),
                "refreshing unchanged mounted metadata preserves a valid open menu request");
            UiText.Initialize("en"); await SettleAsync();
            Check(change.Text == "Relink folder" && unmount.Text == "Unlink folder" && name.Text == "中英文开发项目" &&
                ToolTipService.GetToolTip(open) as string == movedFolder, "English changes menu labels immediately while preserving the user's folder name and full path");
            Check(AutomationProperties.GetName(open).Contains(movedFolder, StringComparison.Ordinal) &&
                AutomationProperties.GetName(more) == UiText.Get("项目操作"), "mounted controls expose localized keyboard and accessibility names");
            _layout.ColumnDefinitions[1].Width = new GridLength(190); await SettleAsync();
            Check(header.ActualWidth <= 162 && name.TextTrimming == TextTrimming.CharacterEllipsis && more.ActualWidth < header.ActualWidth,
                "a narrow sidebar trims the directory name while retaining the menu button");
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "mounted-header-narrow-en.png"));
            _layout.ColumnDefinitions[1].Width = new GridLength(290);
            string driveRoot = Path.GetPathRoot(folderA)!; header.ShowProject(projectA, driveRoot);
            Check(name.Text == driveRoot, "a mounted drive root has a visible directory label");
            header.ShowProject(null, null);
            Check(!header.HasMountedFolder && header.Visibility == Visibility.Collapsed && name.Text == "" && ToolTipService.GetToolTip(open) is null,
                "ordinary chats clear the previous mounted name and path");
            header.ShowProject(projectB, movedFolder); await SettleAsync(); await ShowMenuAsync();
            int actionCount = opened.Count + changed.Count + unmounted.Count; string oldChangeText = change.Text;
            header.Dispose(); UiText.Initialize("zh-CN"); header.ShowProject(projectA, folderA);
            NativeUi.Invoke(open); InvokeMountedMenuItem(unmount); await SettleAsync();
            Check(!header.HasMountedFolder && actionCount == opened.Count + changed.Count + unmounted.Count && change.Text == oldChangeText,
                "disposal cancels menu actions and removes the language subscription");
        }
        finally { header.Dispose(); _layout.Children.Remove(header); _layout.ColumnDefinitions[1].Width = new GridLength(290); UiText.Initialize("zh-CN"); }
    }

    private static void InvokeMountedMenuItem(MenuFlyoutItem item)
    {
        var peer = FrameworkElementAutomationPeer.CreatePeerForElement(item);
        if (peer?.GetPattern(PatternInterface.Invoke) is not IInvokeProvider invoke)
            throw new InvalidOperationException("Mounted menu does not expose native Invoke automation.");
        invoke.Invoke();
    }
}
