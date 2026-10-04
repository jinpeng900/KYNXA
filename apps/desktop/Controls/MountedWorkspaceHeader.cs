using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Controls;

public sealed record MountedWorkspaceRequest(Guid ProjectId, string FolderPath);

/// <summary>Displays the selected project's formal folder. Folder access and catalog writes belong to the page.</summary>
public sealed class MountedWorkspaceHeader : Grid, IDisposable
{
    private readonly TextBlock _name = new() { Name = "MountedWorkspaceName", TextTrimming = TextTrimming.CharacterEllipsis,
        VerticalAlignment = VerticalAlignment.Center };
    private readonly Button _open = new() { Name = "MountedWorkspaceOpen", MinWidth = 0, Padding = new Thickness(5, 4, 5, 4),
        HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Stretch };
    private readonly Button _more = new() { Name = "MountedWorkspaceMore", MinWidth = 0, MinHeight = 0,
        Content = new FontIcon { Glyph = "\uE712", FontSize = 12 } };
    private readonly MenuFlyout _menu = new() { Placement = FlyoutPlacementMode.BottomEdgeAlignedRight };
    private readonly MenuFlyoutItem _change = new() { Name = "MountedWorkspaceChange", Icon = new FontIcon { Glyph = "\uE8F4" } };
    private readonly MenuFlyoutItem _unmount = new() { Name = "MountedWorkspaceUnmount", Icon = new FontIcon { Glyph = "\uE8F4" } };
    private MountedWorkspaceRequest? _current, _menuRequest;
    private bool _disposed;

    public bool HasMountedFolder => _current is not null;
    public event EventHandler<MountedWorkspaceRequest>? OpenRequested;
    public event EventHandler<MountedWorkspaceRequest>? ChangeRequested;
    public event EventHandler<MountedWorkspaceRequest>? UnmountRequested;

    public MountedWorkspaceHeader()
    {
        Visibility = Visibility.Collapsed;
        ColumnSpacing = 3;
        ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        var label = new Grid { ColumnSpacing = 7 };
        label.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        label.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        var icon = new FontIcon { Glyph = "\uE8B7", FontSize = 13, VerticalAlignment = VerticalAlignment.Center };
        label.Children.Add(icon); Grid.SetColumn(_name, 1); label.Children.Add(_name);
        _open.Content = label;
        var resources = Application.Current.Resources;
        if (resources.TryGetValue("KynxaQuietButtonStyle", out var buttonStyle) && buttonStyle is Style quiet) _open.Style = quiet;
        if (resources.TryGetValue("KynxaCompactIconButtonStyle", out var iconStyle) && iconStyle is Style compact) _more.Style = compact;
        if (resources.TryGetValue("KynxaCaptionFontSize", out var size) && size is double caption) _name.FontSize = caption;
        if (resources.TryGetValue("KynxaSecondaryTextBrush", out var brush) && brush is Brush secondary)
            _name.Foreground = icon.Foreground = _more.Foreground = secondary;
        if (resources.TryGetValue("KynxaProjectMenuPresenterStyle", out var presenter) && presenter is Style menuStyle)
            _menu.MenuFlyoutPresenterStyle = menuStyle;
        if (resources.TryGetValue("KynxaWorkMenuItemStyle", out var itemStyle) && itemStyle is Style item)
            _change.Style = _unmount.Style = item;
        _menu.Items.Add(_change); _menu.Items.Add(_unmount); _more.Flyout = _menu;
        _menu.Opening += (_, _) => _menuRequest = _current;
        _menu.Closed += (_, _) => _menuRequest = null;
        _open.Click += (_, _) => { if (!_disposed && _current is { } request) OpenRequested?.Invoke(this, request); };
        _change.Click += (_, _) => Request(ChangeRequested);
        _unmount.Click += (_, _) => Request(UnmountRequested);
        Children.Add(_open); Grid.SetColumn(_more, 1); Children.Add(_more);
        UiText.LanguageChanged += LanguageChanged;
        RefreshLanguage();
    }

    public void ShowProject(Guid? projectId, string? folderPath)
    {
        if (_disposed) return;
        _current = projectId is { } id && id != Guid.Empty && !string.IsNullOrWhiteSpace(folderPath) && Path.IsPathFullyQualified(folderPath)
            ? new(id, folderPath) : null;
        if (_menuRequest != _current) _menu.Hide();
        _name.Text = _current is null ? "" : FolderName(_current.FolderPath);
        ToolTipService.SetToolTip(_open, _current?.FolderPath);
        ToolTipService.SetToolTip(this, _current?.FolderPath);
        Visibility = HasMountedFolder ? Visibility.Visible : Visibility.Collapsed;
        RefreshLanguage();
    }

    private static string FolderName(string path)
    { string name = Path.GetFileName(Path.TrimEndingDirectorySeparator(path)); return name.Length == 0 ? path : name; }
    private void Request(EventHandler<MountedWorkspaceRequest>? handler)
    { if (!_disposed && _menuRequest is { } request && request == _current) handler?.Invoke(this, request); }
    private void LanguageChanged(object? sender, EventArgs args)
    { if (_disposed) return; if (DispatcherQueue.HasThreadAccess) RefreshLanguage(); else DispatcherQueue.TryEnqueue(RefreshLanguage); }
    private void RefreshLanguage()
    {
        if (_disposed) return;
        _change.Text = UiText.Get("重新关联文件夹"); _unmount.Text = UiText.Get("取消关联文件夹");
        string open = _current is null ? UiText.Get("在文件资源管理器中打开") : string.Format(UiText.Get("打开挂载文件夹：{0}"), _current.FolderPath);
        AutomationProperties.SetName(_open, open);
        AutomationProperties.SetName(_more, UiText.Get("项目操作"));
        ToolTipService.SetToolTip(_more, UiText.Get("项目操作"));
    }

    public void Dispose()
    { if (_disposed) return; _disposed = true; _menu.Hide(); _current = _menuRequest = null; UiText.LanguageChanged -= LanguageChanged; }
}
