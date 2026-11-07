using System.Globalization;

namespace KYNXA_Desktop.Services;

internal static class LocalModelDownloadPresentation
{
    public static string Describe(LocalModelDownloadInfo? download)
    {
        if (download is null) return "";
        string phase = download.Phase switch
        {
            "queued" => UiText.Get("等待下载连接"), "connecting" => UiText.Get("正在连接下载源"),
            "switching-source" => UiText.Get("正在切换下载源"), "verifying" => UiText.Get("正在校验完整性"), _ => ""
        };
        string kind = download.SourceKind switch { "mirror" => UiText.Get("镜像"), "official" => UiText.Get("官方"), "github" => "GitHub", _ => "" };
        string source = string.IsNullOrWhiteSpace(download.Source) ? "" : string.Join(" ", new[] { kind, download.Source }.Where(s => s.Length > 0));
        bool transferring = download.Phase == "downloading";
        string speed = !transferring ? "" : download.BytesPerSecond is long rate && rate >= 0
            ? (rate >= 1024 * 1024 ? (rate / (1024d * 1024)).ToString("0.#", CultureInfo.CurrentCulture) + " MiB/s"
                : (rate / 1024d).ToString("0.#", CultureInfo.CurrentCulture) + " KiB/s") : UiText.Get("正在测速");
        string remaining = transferring ? Remaining(download.RemainingSeconds) : "";
        string connections = transferring && download.Connections > 1
            ? string.Format(UiText.Get("{0} 路下载"), download.Connections) : "";
        string adjustment = !transferring ? "" : download.Adjustment switch
        {
            "probing" => string.Format(UiText.Get("自适应目标 {0} 路"), download.TargetConnections),
            "no-gain" => UiText.Get("增并发无提速，已回退"),
            "backoff" => UiText.Get("限流或连接异常，降并发退避"),
            "chat-priority" => UiText.Get("优先聊天，下载降并发"),
            "resource-limited" => UiText.Get("按可用资源调整并发"), _ => ""
        };
        return string.Join(" · ", new[] { phase, source, speed, remaining, connections, adjustment }.Where(s => s.Length > 0));
    }

    private static string Remaining(long? seconds) => seconds switch
    {
        null or < 0 => UiText.Get("剩余时间估算中"),
        < 60 => string.Format(UiText.Get("约 {0} 秒"), seconds),
        < 3600 => string.Format(UiText.Get("约 {0} 分钟"), (seconds.Value + 59) / 60),
        _ => string.Format(UiText.Get("约 {0} 小时 {1} 分钟"), seconds.Value / 3600, seconds.Value % 3600 / 60)
    };
}
