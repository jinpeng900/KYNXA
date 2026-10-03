using Microsoft.UI.Xaml;
using KYNXA_Desktop.Views;

// To learn more about WinUI, the WinUI project structure,
// and more about our project templates, see: http://aka.ms/winui-project-info.

namespace KYNXA_Desktop;

/// <summary>The application window hosting the KYNXA shell.</summary>
public sealed partial class MainWindow : Window
{
    private XamlRoot? _windowXamlRoot;

    private void UpdateWindowMinimumSize()
    {
        if (AppWindow.Presenter is not Microsoft.UI.Windowing.OverlappedPresenter presenter) return;
        double scale = _windowXamlRoot?.RasterizationScale ?? 1;
        // Windowing dimensions are pixels; keep the minimum usable size consistent with XAML scaling.
        presenter.PreferredMinimumWidth = (int)Math.Ceiling(480 * scale);
        presenter.PreferredMinimumHeight = (int)Math.Ceiling(360 * scale);
    }

    private void RootFrame_Loaded(object sender, RoutedEventArgs e)
    {
        if (_windowXamlRoot is not null) return;
        _windowXamlRoot = RootFrame.XamlRoot;
        if (_windowXamlRoot is null) return;
        _windowXamlRoot.Changed += WindowXamlRoot_Changed;
        UpdateWindowMinimumSize();
    }

    private void WindowXamlRoot_Changed(XamlRoot sender, XamlRootChangedEventArgs args) => UpdateWindowMinimumSize();

    private void NavigationMenu_Click(object sender, RoutedEventArgs e)
    {
        if (RootFrame.Content is ShellPage page && sender is FrameworkElement anchor) page.ShowNavigationMenu(anchor);
    }

    public MainWindow()
    {
        InitializeComponent();

        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);

        AppWindow.SetIcon("Assets/AppIcon.ico");
        UpdateWindowMinimumSize();
        RootFrame.Loaded += RootFrame_Loaded;
        Closed += (_, _) =>
        {
            if (_windowXamlRoot is not null) _windowXamlRoot.Changed -= WindowXamlRoot_Changed;
        };
        AppWindow.Resize(new Windows.Graphics.SizeInt32(1440, 900));

        if (Microsoft.UI.Windowing.AppWindowTitleBar.IsCustomizationSupported())
        {
            AppWindow.TitleBar.ButtonBackgroundColor = Microsoft.UI.Colors.Transparent;
            AppWindow.TitleBar.ButtonInactiveBackgroundColor = Microsoft.UI.Colors.Transparent;
        }

        RootFrame.Navigate(typeof(ShellPage));
    }
}
