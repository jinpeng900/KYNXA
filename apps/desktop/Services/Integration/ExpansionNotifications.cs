namespace KYNXA_Desktop.Services;

/// <summary>Completion balloons are opt-in and persist only inside this expansion's data directory.</summary>
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
        if (enabled) File.WriteAllText(PreferencePath, "enabled");
        else File.Delete(PreferencePath);
        return enabled;
    }

    public static void ReplyCompleted(string? title)
    {
        if (Enabled && App.Window is MainWindow window) window.ShowCompletionNotification(title);
    }
}
