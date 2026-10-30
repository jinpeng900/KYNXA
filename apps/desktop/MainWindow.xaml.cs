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
    private bool _closeDecisionPending;

    internal void RequestExit() => _trayService.RequestExit();
    internal void RestoreFromActivation() => _trayService.Restore();
    internal void ShowCompletionNotification(string? title) => _trayService.ShowCompletionNotification(title);

    private async void MainWindowCloseRequested(object? sender, EventArgs args)
    {
        if (_closeDecisionPending || RootFrame.Content is not ShellPage page) return;
        _closeDecisionPending = true;
        try
        {
            // Choose before entering the existing exit pipeline; tray hiding must leave running tasks untouched.
            // 在进入原有退出流程之前选择行为；隐藏到托盘不能触发正在运行任务的退出处理。
            string? action = await page.ConfirmMainWindowCloseAsync();
            if (action == Models.UI.MainWindowClosePreferences.Tray)
            {
                if (!_trayService.TryHideToTray()) page.ShowMainWindowCloseFeedback("系统托盘暂时不可用，窗口已保留。");
            }
            else if (action == Models.UI.MainWindowClosePreferences.Exit) RequestExit();
        }
        catch (Exception error)
        {
            System.Diagnostics.Debug.WriteLine($"Main window close choice: {error.GetType().Name}");
            page.ShowMainWindowCloseFeedback("暂时无法显示关闭确认，请先关闭其他弹窗。");
        }
        finally { _closeDecisionPending = false; }
    }

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
        _trayService.CloseRequested += MainWindowCloseRequested;
    }
}
