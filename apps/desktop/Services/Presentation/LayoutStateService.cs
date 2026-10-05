using System.Text.Json;
using Windows.Storage;
using KYNXA_Desktop.Models.UI;

namespace KYNXA_Desktop.Services;

public sealed class LayoutStateService
{
    private const string SettingsKey = "kynxa.ui.layout.v2";
    private static string SettingsPath => Path.Combine(StoragePaths.DesktopDirectory, "layout.json");

    public LayoutState Load()
    {
        try
        {
            string? json = null;
            if (File.Exists(SettingsPath)) json = File.ReadAllText(SettingsPath);
            else if (ApplicationData.Current.LocalSettings.Values[SettingsKey] is string legacy) json = legacy;
            if (json is not null)
            {
                LayoutState? state = JsonSerializer.Deserialize<LayoutState>(json);
                if (state is not null && state.LayoutVersion <= LayoutState.CurrentVersion)
                {
                    state.RecentWorkChatIds ??= [];
                    state.InterfaceLanguage = UiText.NormalizeLanguage(state.InterfaceLanguage);
                    if (state.LayoutVersion < 3)
                    {
                        state.SidebarWidth = Layout.ShellLayoutMetrics.SidebarDefault;
                    }
                    if (state.LayoutVersion < 4)
                    {
                        state.PreviewVisible = true;
                        state.PreviewWidth = 0;
                    }
                    state.LayoutVersion = LayoutState.CurrentVersion;
                    if (!File.Exists(SettingsPath)) Save(state);
                    return state;
                }
            }
        }
        catch
        {
            // A damaged preference must never prevent the shell from starting.
            // 偏好文件损坏不能阻止主界面启动。
        }

        return LayoutState.CreateDefault();
    }

    public bool Save(LayoutState state)
    {
        if (StoragePaths.IsMigrating) return false;
        try
        {
            File.WriteAllText(SettingsPath + ".tmp", JsonSerializer.Serialize(state));
            File.Move(SettingsPath + ".tmp", SettingsPath, overwrite: true);
            return true;
        }
        catch
        {
            // Layout persistence is best-effort; the in-memory layout remains usable.
            // 布局保存尽力完成；保存失败时内存布局仍可使用。
            return false;
        }
    }
}
