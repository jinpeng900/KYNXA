using Microsoft.UI.Xaml;
using System.Text.Json;

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
                // 配置的数据位置本身不可用时，仍尽量保留启动诊断。
                try { File.WriteAllText(Path.Combine(Windows.Storage.ApplicationData.Current.LocalFolder.Path,
                    "last-ui-error.txt"), error.Exception.ToString()); }
                catch { /* Diagnostics must not obscure the original exception. 中文：诊断写入失败不能掩盖原始异常。 */ }
            }
        };
    }

    protected override void OnLaunched(Microsoft.UI.Xaml.LaunchActivatedEventArgs args)
    {
        // Restore language and appearance before creating controls to avoid a different first frame.
        // 创建控件前恢复语言与外观，避免首帧使用不同的配色。
        var preferences = new Services.LayoutStateService().Load();
        Services.UiText.Initialize(preferences.InterfaceLanguage);
        Services.AppearanceService.ApplyPreferences(preferences.AppearancePaletteId, preferences.AppearanceMode, preferences.ChatFontSize, preferences.AppearanceAccent);
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
