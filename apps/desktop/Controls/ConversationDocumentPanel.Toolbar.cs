using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop.Controls;

public sealed partial class ConversationDocumentPanel
{
    private readonly Button _previewOptions = QuietIcon("\uE712", "更多预览操作");
    private readonly TextBlock _previewCaption = new()
    {
        FontSize = 12, Margin = new Thickness(2, 0, 4, 0),
        VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis
    };
    private bool _toolbarWebsite, _canShowSource, _hasMorePreview;

    private void InitializePreviewToolbar()
    {
        // Keep the address flexible; secondary actions share one menu at every sidebar width.
        // 地址栏始终可伸展；次要操作归入同一个菜单，窄侧栏也不会挤掉地址。
        // Group navigation so collapsed history buttons leave no empty grid gaps in file previews.
        // 导航共用一列；文件预览隐藏前进后退时，不保留空列间距。
        var navigation = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 4 };
        foreach (var button in new[] { _back, _forward, _reload }) navigation.Children.Add(button);
        for (int column = 0; column < 3; column++)
            _toolbar.ColumnDefinitions.Add(new() { Width = column == 1 ? new GridLength(1, GridUnitType.Star) : GridLength.Auto });
        AddToolbar(navigation, 0);
        AddToolbar(_address, 1); AddToolbar(_previewCaption, 1); AddToolbar(_previewOptions, 2);
        foreach (var button in new[] { _back, _forward, _reload, _previewOptions })
        {
            button.Width = button.Height = 32;
            button.Padding = new Thickness(0);
            button.HorizontalContentAlignment = HorizontalAlignment.Center;
            button.VerticalContentAlignment = VerticalAlignment.Center;
        }
        _address.VerticalAlignment = VerticalAlignment.Center;
        _previewOptions.Click += (_, _) => ShowPreviewOptions();
    }

    private void UpdatePreviewCaption(string? title)
    {
        _previewCaption.Text = string.IsNullOrWhiteSpace(title) ? Path.GetFileName(_path ?? "") : title;
        ToolTipService.SetToolTip(_previewCaption, _path ?? title);
    }

    private void ShowPreviewOptions()
    {
        if (_disposed) return;
        string? selectedKey = _activeKey;
        Guid conversationId = _conversationId;
        bool StillSelected() => !_disposed && _activeKey == selectedKey && _conversationId == conversationId;
        var menu = new MenuFlyout();
        if (Application.Current.Resources.TryGetValue("KynxaProjectMenuPresenterStyle", out var value) && value is Style style)
            menu.MenuFlyoutPresenterStyle = style;
        if (!_toolbarWebsite)
        {
            AddPreviewMode(menu, "宽屏预览", double.NaN, StillSelected);
            AddPreviewMode(menu, "手机尺寸预览", 390, StillSelected);
            if (_canShowSource || _hasMorePreview) menu.Items.Add(new MenuFlyoutSeparator());
            if (_canShowSource)
            {
                var source = PreviewMenuItem(_showSource ? "显示预览" : "查看源码", "\uE943");
                source.Click += async (_, _) => { if (StillSelected()) await ToggleSourceAsync(); };
                menu.Items.Add(source);
            }
            if (_hasMorePreview)
            {
                var more = PreviewMenuItem("继续加载", "\uE74B");
                more.IsEnabled = _activeKey is { } key && _documentWork.TryGetValue(key, out var work) && !work.LoadingMore;
                more.Click += async (_, _) => { if (StillSelected()) await LoadMoreAsync(); };
                menu.Items.Add(more);
            }
        }
        else
        {
            var website = new MenuFlyoutSubItem { Text = UiText.Get("网站数据"), Icon = new FontIcon { Glyph = "\uE713" } };
            var clear = PreviewMenuItem("清除所有网站登录数据", "\uE74D");
            clear.IsEnabled = !_clearingWebsiteData;
            clear.Click += async (_, _) => { if (StillSelected()) await ClearWebsiteDataAsync(); };
            website.Items.Add(clear); menu.Items.Add(website);
        }
        menu.Items.Add(new MenuFlyoutSeparator());
        var workbench = PreviewMenuItem("打开工作台", "\uE80F");
        workbench.Click += (_, _) => { if (StillSelected()) WorkbenchRequested?.Invoke(this, EventArgs.Empty); };
        menu.Items.Add(workbench);
        menu.ShowAt(_previewOptions);
    }

    private void AddPreviewMode(MenuFlyout menu, string text, double width, Func<bool> stillSelected)
    {
        var mode = new ToggleMenuFlyoutItem
        {
            Text = UiText.Get(text), IsChecked = double.IsNaN(width) ? double.IsNaN(_previewWidth) : _previewWidth == width
        };
        mode.Click += (_, _) => { if (stillSelected()) SetPreviewWidth(width); };
        menu.Items.Add(mode);
    }

    private static MenuFlyoutItem PreviewMenuItem(string text, string glyph) => new()
    {
        Text = UiText.Get(text), Icon = new FontIcon { Glyph = glyph, FontSize = 14 }
    };
}
