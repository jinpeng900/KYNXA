using Microsoft.UI.Xaml;
using KYNXA_Desktop.Views;

// To learn more about WinUI, the WinUI project structure,
// and more about our project templates, see: http://aka.ms/winui-project-info.
// WinUI 项目结构和模板的官方说明见上方链接。

namespace KYNXA_Desktop;

/// <summary>
/// The application window hosting the KYNXA shell.
/// 承载 KYNXA 主界面的应用窗口。
/// </summary>
public sealed partial class MainWindow : Window
{
    private readonly Services.MainWindowTrayService _trayService;

    internal void RequestExit() => _trayService.RequestExit();
    internal void ShowCompletionNotification(string? title) => _trayService.ShowCompletionNotification(title);

    private void MoreOptions_Click(object sender, RoutedEventArgs e)
    {
        if (RootFrame.Content is ShellPage page && sender is FrameworkElement anchor)
            page.ShowNavigationMenu(anchor);
    }

    public MainWindow()
    {
        InitializeComponent();

        Title = AppCaptionText.Text = Services.AppIdentity.DisplayName;

        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);

        AppWindow.SetIcon("Assets/AppIcon.ico");
        AppWindow.Resize(new Windows.Graphics.SizeInt32(1440, 900));

        if (Microsoft.UI.Windowing.AppWindowTitleBar.IsCustomizationSupported())
        {
            AppWindow.TitleBar.ButtonBackgroundColor = Microsoft.UI.Colors.Transparent;
            AppWindow.TitleBar.ButtonInactiveBackgroundColor = Microsoft.UI.Colors.Transparent;
        }

        RootFrame.Navigate(typeof(ShellPage));
        Services.AppearanceService.TrackWindow(this);
        _trayService = new Services.MainWindowTrayService(this);
    }
}
