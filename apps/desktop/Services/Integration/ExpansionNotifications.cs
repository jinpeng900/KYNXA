using System.Diagnostics;
using Microsoft.Windows.AppLifecycle;
using Microsoft.Windows.AppNotifications;
using Microsoft.Windows.AppNotifications.Builder;

namespace KYNXA_Desktop.Services;

/// <summary>
/// 回复完成通知由用户主动开启，偏好仅保存到当前桌面数据目录。
/// </summary>
internal static class ExpansionNotifications
{
    private const string ReplyCompletedAction = "reply-completed";
    private static AppNotificationManager? _manager;

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
            // 写入成功后再发布开启标记，保存失败时保留原开关状态。
            File.WriteAllText(PreferencePath + ".tmp", "enabled");
            File.Move(PreferencePath + ".tmp", PreferencePath, overwrite: true);
        }
        else File.Delete(PreferencePath);
        return enabled;
    }

    public static void Register(MainWindow window)
    {
        var dispatcher = window.DispatcherQueue;
        try
        {
            if (!AppNotificationManager.IsSupported()) return;
            var manager = AppNotificationManager.Default;
            manager.NotificationInvoked += NotificationInvoked;
            try { manager.Register(); }
            catch
            {
                manager.NotificationInvoked -= NotificationInvoked;
                throw;
            }
            _manager = manager;
            window.Closed += (_, _) =>
            {
                _manager = null;
                manager.NotificationInvoked -= NotificationInvoked;
                try { manager.Unregister(); }
                catch (Exception error) { Debug.WriteLine($"Notification shutdown: {error.GetType().Name}"); }
            };
        }
        catch (Exception error)
        {
            // Registration failure must leave chat and tray startup usable.
            // 系统通知注册失败时，聊天与托盘仍可正常启动。
            Debug.WriteLine($"Notification registration: {error.GetType().Name}");
        }

        void NotificationInvoked(AppNotificationManager sender, AppNotificationActivatedEventArgs args)
        {
            if (TryReadConversationId(args.Arguments, out Guid conversationId))
                dispatcher.TryEnqueue(() => window.OpenReplyNotification(conversationId));
        }
    }

    public static bool TryActivate(MainWindow window, AppActivationArguments? activation)
    {
        if (activation?.Kind != ExtendedActivationKind.AppNotification ||
            activation.Data is not AppNotificationActivatedEventArgs args ||
            !TryReadConversationId(args.Arguments, out Guid conversationId)) return false;
        window.OpenReplyNotification(conversationId);
        return true;
    }

    internal static bool TryReadConversationId(IDictionary<string, string> arguments, out Guid conversationId)
    {
        conversationId = Guid.Empty;
        return arguments.TryGetValue("action", out string? action) && action == ReplyCompletedAction &&
            arguments.TryGetValue("conversationId", out string? value) && Guid.TryParseExact(value, "D", out conversationId) &&
            conversationId != Guid.Empty;
    }

    internal static AppNotification BuildCompletionNotification(Guid conversationId, string? title)
    {
        string body = string.IsNullOrWhiteSpace(title) ? UiText.Get("可以返回应用查看回复。") :
            title.Length > 180 ? title[..(char.IsHighSurrogate(title[179]) ? 179 : 180)] : title;
        return new AppNotificationBuilder()
            .AddArgument("action", ReplyCompletedAction)
            .AddArgument("conversationId", conversationId.ToString("D"))
            .AddText(UiText.Get("回复已完成"))
            .AddText(body)
            .SetAppLogoOverride(new Uri(Path.Combine(AppContext.BaseDirectory, "Assets", "StoreLogo.png")))
            .MuteAudio()
            .BuildNotification();
    }

    public static void ReplyCompleted(Guid conversationId, string? title)
    {
        if (_manager is not { } manager || App.Window is not MainWindow window ||
            !window.CanShowReplyNotification || !Enabled) return;
        try { manager.Show(BuildCompletionNotification(conversationId, title)); }
        catch (Exception error)
        {
            // A Windows notification failure must not interrupt a completed reply or its queued prompts.
            // 系统通知失败不能影响已完成的回复或后续消息队列。
            Debug.WriteLine($"Reply notification delivery: {error.GetType().Name}");
        }
    }
}
