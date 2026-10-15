using Microsoft.UI.Xaml;
using System.Text.Json;

namespace KYNXA_Desktop;

public partial class App : Application
{
    public static Window Window { get; private set; } = null!;

    public App()
    {
        ConfigurePersistentStorage();
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
                // 配置的数据位置本身不可用时，仍尽量保留启动诊断。
                try { File.WriteAllText(Path.Combine(Windows.Storage.ApplicationData.Current.LocalFolder.Path,
                    "last-ui-error.txt"), error.Exception.ToString()); }
                catch { /* Diagnostics must not obscure the original exception. 中文：诊断写入失败不能掩盖原始异常。 */ }
            }
        };
    }

    private static void ConfigurePersistentStorage()
    {
        // Isolated package previews explicitly opt into a portable application root.
        // 独立包预览显式指定便携应用根目录，不读取正式应用的用户指针。
        if (!string.IsNullOrWhiteSpace(Environment.GetEnvironmentVariable("KYNXA_APPLICATION_ROOT"))) return;
        try
        {
            _ = Windows.ApplicationModel.Package.Current.Id;
        }
        catch (InvalidOperationException)
        {
            // Unpackaged builds keep upstream's portable folder behavior.
            // 未打包版本继续使用上游的便携目录行为。
            return;
        }
        string profile = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".kynxa");
        string legacyModels = Path.Combine(profile, "models");
        string legacyDesktop = Windows.Storage.ApplicationData.Current.LocalFolder.Path;
        bool hasLegacyDesktop = Directory.EnumerateFiles(legacyDesktop, "*.json").Any();
        Services.ApplicationPaths.ConfigurePackagedStorage(
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "KYNXA"), profile,
            Directory.Exists(legacyModels) || hasLegacyDesktop ? legacyModels : null,
            hasLegacyDesktop ? legacyDesktop : null);
    }

    protected override void OnLaunched(Microsoft.UI.Xaml.LaunchActivatedEventArgs args)
    {
        // Restore language and appearance before creating controls to avoid a different first frame.
        // 创建控件前恢复语言与外观，避免首帧使用不同的配色。
        var preferences = new Services.LayoutStateService().Load();
        Services.UiText.Initialize(preferences.InterfaceLanguage);
        Services.AppearanceService.ApplyPreferences(preferences.AppearancePaletteId, preferences.AppearanceMode, preferences.ChatFontSize, preferences.AppearanceAccent);
        Services.ModelGatewayService.LegacyDesktopDirectory = Services.ApplicationPaths.LegacyDesktopDirectory;
        string? configuredModelHome = Environment.GetEnvironmentVariable("KYNXA_MODEL_HOME");
        if (!string.IsNullOrWhiteSpace(configuredModelHome))
        {
            string modelHome = Services.ApplicationPaths.Resolve(configuredModelHome);
            Services.ExtensionPaths.LegacyRoot = string.Equals(Path.GetFileName(modelHome), "Models", StringComparison.OrdinalIgnoreCase)
                ? Path.GetDirectoryName(modelHome)! : Path.Combine(modelHome, "Conversations");
        }
        else Services.ExtensionPaths.LegacyRoot = Services.StoragePaths.EnvironmentControlled
            ? Services.StoragePaths.DataRoot : Services.ApplicationPaths.LegacyExtensionRoot;
        Services.ExtensionPaths.Reload();
        Window = new MainWindow();
        Window.Activate();
        _ = WarmUpModelGatewayAsync();
    }

    private static async Task WarmUpModelGatewayAsync()
    {
        Services.ModelChoice? choice = null;
        try
        {
            choice = await Task.Run(() => new Services.ModelSelectionStore(Services.StoragePaths.DesktopDirectory).Load());
            if (choice is not null && (string.IsNullOrWhiteSpace(choice.ProviderId) || string.IsNullOrWhiteSpace(choice.ModelId)))
                choice = null;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or JsonException)
        {
            // UI preferences belong to application startup; API services receive only stable model identities.
            // UI 偏好由应用启动读取；API 服务仅接收稳定模型身份，偏好失败不能阻止连接或聊天。
            System.Diagnostics.Debug.WriteLine("模型选择偏好不可读，继续缺省准备 / Model selection preference is unreadable; continue default preparation.");
        }
        try { await Services.ModelGatewayService.WarmUpModelsAsync(choice?.ProviderId, choice?.ModelId); }
        catch (Exception error)
        {
            // The API client retries and shows the actionable error when a model is used.
            // 实际使用模型时由 API 客户端重试，并展示可采取行动的错误信息。
            System.Diagnostics.Debug.WriteLine($"Model gateway startup: {error.Message}");
        }
    }
}
