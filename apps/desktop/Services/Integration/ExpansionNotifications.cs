namespace KYNXA_Desktop.Services;

/// <summary>
/// Completion balloons are opt-in and persist only inside the current desktop data directory.
/// 回复完成通知由用户主动开启，偏好仅保存到当前桌面数据目录。
/// </summary>
internal static class ExpansionNotifications
{
    private static string PreferencePath => Path.Combine(StoragePaths.DesktopDirectory, "completion-notifications.enabled");
    public static bool Enabled => File.Exists(PreferencePath);

    public static bool Toggle()
    {
        return SetEnabled(!Enabled);
    }

    public static bool SetEnabled(bool enabled)
    {
        if (enabled)
        {
            // Publish the enabled marker only after its write succeeds, retaining the previous state on failure.
            // 写入成功后再发布开启标记，保存失败时保留原开关状态。
            File.WriteAllText(PreferencePath + ".tmp", "enabled");
            File.Move(PreferencePath + ".tmp", PreferencePath, overwrite: true);
        }
        else File.Delete(PreferencePath);
        return enabled;
    }

    public static void ReplyCompleted(string? title)
    {
        if (Enabled && App.Window is MainWindow window) window.ShowCompletionNotification(title);
    }
}
