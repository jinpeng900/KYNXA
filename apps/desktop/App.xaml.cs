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
        // Restore interface language before creating controls; settings can change it live.
        Services.UiText.Initialize(new Services.LayoutStateService().Load().InterfaceLanguage);
        Services.ModelGatewayService.LegacyDesktopDirectory = Services.StoragePaths.DataRoot is null
            ? Services.StoragePaths.DesktopDirectory : null;
        string? configuredModelHome = Environment.GetEnvironmentVariable("KYNXA_MODEL_HOME");
        if (!string.IsNullOrWhiteSpace(configuredModelHome))
        {
            string modelHome = Path.TrimEndingDirectorySeparator(Path.GetFullPath(configuredModelHome));
            Services.ExtensionPaths.LegacyRoot = string.Equals(Path.GetFileName(modelHome), "Models", StringComparison.OrdinalIgnoreCase)
                ? Path.GetDirectoryName(modelHome)! : Path.Combine(modelHome, "Conversations");
        }
        else Services.ExtensionPaths.LegacyRoot = Services.StoragePaths.DataRoot
            ?? Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".kynxa");
        Services.ExtensionPaths.Reload();
        Window = new MainWindow();
        Window.Activate();
        _ = WarmUpModelGatewayAsync();
    }

    private static async Task WarmUpModelGatewayAsync()
    {
        try { await Services.ModelGatewayService.EnsureReadyAsync(); }
        catch (Exception error)
        {
            // The API client retries and shows the actionable error when a model is used.
            System.Diagnostics.Debug.WriteLine($"Model gateway startup: {error.Message}");
        }
    }
}
