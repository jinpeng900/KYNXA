using Microsoft.UI.Xaml;

namespace KYNXA_Desktop;

public partial class App : Application
{
    public static Window Window { get; private set; } = null!;

    public App()
    {
        InitializeComponent();
        UnhandledException += (_, error) =>
        {
            try
            {
                File.WriteAllText(Path.Combine(Services.StoragePaths.DesktopDirectory, "last-ui-error.txt"), error.Exception.ToString());
            }
            catch
            {
                // If the configured storage location itself fails, preserve startup diagnostics.
                try { File.WriteAllText(Path.Combine(Windows.Storage.ApplicationData.Current.LocalFolder.Path,
                    "last-ui-error.txt"), error.Exception.ToString()); }
                catch { /* Diagnostics must not obscure the original exception. */ }
            }
        };
    }

    protected override void OnLaunched(Microsoft.UI.Xaml.LaunchActivatedEventArgs args)
    {
        Window = new MainWindow();
        Window.Activate();
    }
}
