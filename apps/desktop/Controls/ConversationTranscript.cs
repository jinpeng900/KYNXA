using System.ComponentModel;
using System.Diagnostics;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.Web.WebView2.Core;
using Windows.ApplicationModel.DataTransfer;
using Windows.System;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// A single document keeps browser selection continuous across the whole conversation.
/// 整个聊天使用同一文档，保证浏览器选择可以跨消息连续进行。
/// </summary>
public sealed class ConversationTranscript : Grid, IDisposable
{
    private const string HostName = "kynxa-transcript.local";
    private const string PageUrl = "https://" + HostName + "/Transcript/index.html";
    private readonly WebView2 _browser = new() { DefaultBackgroundColor = AppearanceService.ParseColor(AppearanceService.Current.Main) };
    private readonly TextBlock _notice = new() { Text = UiText.Get("正在加载聊天…"), Margin = new Thickness(12), TextWrapping = TextWrapping.Wrap };
    private readonly TaskCompletionSource _ready = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private readonly DispatcherTimer _refresh = new() { Interval = TimeSpan.FromMilliseconds(40) };
    private readonly Dictionary<Guid, CachedHtml> _html = [];
    private readonly LinkedList<Guid> _cacheOrder = [];
    private readonly Dictionary<Guid, LinkedListNode<Guid>> _cacheNodes = [];
    private readonly Dictionary<(Guid MessageId, string CallId), ToolDisplayRevision> _toolRevisions = [];
    private readonly Queue<(Guid MessageId, string CallId)> _toolRevisionOrder = [];
    private const int MaximumToolDisplayRevisions = 2048;
    private long _nextToolRevision;
    private const int MaximumCachedMessages = 1024;
    private const long MaximumCacheCharacters = 8 * 1024 * 1024;
    private long _cacheCharacters;
    private IReadOnlyList<ConversationMessageViewModel> _messages = [];
    private Guid? _conversationId;
    private CancellationTokenSource? _timeRestoreCancellation;
    private Task? _initialization;
    private string _noticeKey = "正在加载聊天…";
    private long _revision, _generation;
    private bool _openAtBottom, _dirty, _rendering, _disposed;
    private sealed record CachedSegmentHtml(AssistantSegment Source, string Html, string ReasoningHtml)
    {
        public long Size => (long)Source.Content.Length + Source.Reasoning.Length + Html.Length + ReasoningHtml.Length;
    }
    private sealed record CachedHtml(string Role, string Content, string Reasoning, bool Streaming, string Mode, string Html, string ReasoningHtml,
        CachedSegmentHtml[] Segments)
    {
        public long Size => (long)Content.Length + Reasoning.Length + Html.Length + ReasoningHtml.Length + Segments.Sum(segment => segment.Size);
    }
    private sealed record Snapshot(Guid Id, string Role, string Content, string Reasoning, bool Streaming,
        string? ReasoningState, long ReasoningSeconds, bool Waiting, string Status, string Error, bool CanRetry,
        ToolActivity[] ToolActivities, AssistantSegment[] AssistantSegments, string Mode, long DurationMs,
        long? GenerationElapsedMs, long? CreatedAtMs, long? EndRecordedAtMs);
    private sealed record ToolDisplayRevision(WeakReference<ToolActivity> Source, long Revision);

    public Task Ready => _ready.Task;
    internal WebView2 Browser => _browser;
    public event EventHandler<Guid>? RetryRequested;
    public event EventHandler<ToolResultRequest>? ToolResultRequested;
    public event EventHandler? ConversationChanged;

    public ConversationTranscript()
    {
        Children.Add(_browser);
        Children.Add(_notice);
        Loaded += (_, _) => Preload();
        _refresh.Tick += async (_, _) => { _refresh.Stop(); await FlushAsync(); };
        UiText.LanguageChanged += LanguageChanged;
        AppearanceService.Changed += AppearanceChanged;
    }

    public void Preload()
    {
        if (_disposed || !IsLoaded) return;
        _initialization ??= InitializeAsync();
    }

    public void ShowConversation(Guid? conversationId, IReadOnlyList<ConversationMessageViewModel> messages, bool openAtBottom = true)
    {
        if (_disposed) return;
        foreach (var message in _messages) message.PropertyChanged -= MessageChanged;
        bool changed = _conversationId != conversationId;
        _conversationId = conversationId;
        if (changed) ConversationChanged?.Invoke(this, EventArgs.Empty);
        _generation++;
        _messages = messages.ToArray();
        foreach (var message in _messages) message.PropertyChanged += MessageChanged;
        _timeRestoreCancellation?.Cancel();
        _timeRestoreCancellation?.Dispose();
        _timeRestoreCancellation = new();
        if (conversationId is Guid timeScope)
            _ = RestoreMessageTimesAsync(timeScope, _messages.Select(row => row.Message).ToArray(), _generation, _timeRestoreCancellation.Token);
        _openAtBottom |= changed || openAtBottom;
        // The browser can restore a recently visited conversation while updated content is parsed.
        // 解析更新内容期间，浏览器可先恢复最近访问过的聊天。
        if (changed) Post(new { type = "openConversation", conversationId = conversationId?.ToString() ?? "" });
        QueueRefresh();
        // A navigation should not wait for the 40 ms stream batching timer.
        // 切换聊天不应等待 40 毫秒的流式合并刷新计时器。
        if (!_rendering && _ready.Task.IsCompletedSuccessfully) { _refresh.Stop(); _ = FlushAsync(); }
    }

    private async Task RestoreMessageTimesAsync(Guid conversationId, ChatMessageState[] messages, long generation, CancellationToken cancellationToken)
    {
        try
        {
            await MessageTimePresentation.RestoreAsync(conversationId, messages, cancellationToken);
            if (!_disposed && _generation == generation && _conversationId == conversationId) QueueRefresh();
        }
        catch (Exception error)
        {
            // A display cache must never break navigation or expose message content in diagnostics.
            // 显示缓存不能破坏会话切换，也不能在诊断中暴露消息内容。
            Debug.WriteLine($"Message time cache restore: {error.GetType().Name}");
        }
    }

    public void BeforeSend() => Post(new { type = "beforeSend" });
    public void ClearSelection() => Post(new { type = "clearSelection" });
    public void JumpToLatest() => Post(new { type = "jumpToLatest" });
    private void AppearanceChanged(object? sender, EventArgs e)
    {
        if (_disposed) return;
        if (DispatcherQueue.HasThreadAccess) RefreshAppearance();
        else DispatcherQueue.TryEnqueue(RefreshAppearance);
    }

    private void RefreshAppearance()
    {
        if (_disposed) return;
        var palette = AppearanceService.Current;
        _browser.DefaultBackgroundColor = AppearanceService.ParseColor(palette.Main);
        // Appearance updates only CSS colors; message DOM, selection and scroll ownership remain intact.
        // 外观更新只修改 CSS 颜色，保留消息 DOM、选区和滚动归属。
        Post(new
        {
            type = "setAppearance",
            palette = new
            {
                main = palette.Main, soft = palette.Soft, accent = palette.Accent, selection = palette.Selection,
                text = palette.Text, secondary = palette.Secondary, border = palette.Border, sidebar = palette.Sidebar
            }
        });
    }

    private void LanguageChanged(object? sender, EventArgs e)
    {
        if (_disposed) return;
        if (DispatcherQueue.HasThreadAccess) RefreshLanguage();
        else DispatcherQueue.TryEnqueue(RefreshLanguage);
    }

    private void RefreshLanguage()
    {
        if (_disposed) return;
        _notice.Text = UiText.Get(_noticeKey);
        // The browser retains language-neutral metadata for active and cached rows.
        // Do not queue a transcript render: that would also touch selection and scrolling.
        // 浏览器保留当前行与缓存行的语言无关元数据；本地化不重渲染聊天，以免改变选择和滚动。
        Post(new
        {
            type = "initializeUi", language = UiText.Language,
            strings = new
            {
                conversation = UiText.Get("对话"), transcript = UiText.Get("聊天记录"),
                copy = UiText.Get("复制"), copyMessage = UiText.Get("复制整条消息"), retry = UiText.Get("重试"),
                copied = UiText.Get("已复制"), copyFailed = UiText.Get("复制失败，请重试。"), jumpToLatest = UiText.Get("跳转到最新消息"),
                copyBlock = UiText.Get("复制此内容块"), copyCurrentContent = UiText.Get("复制当前内容"), blockPlainText = UiText.Get("文本"),
                messageSentAt = UiText.Get("发送时间"), replyCompletedAt = UiText.Get("回复完成时间"),
                replyEndedAt = UiText.Get("回复结束时间"), timeNotRecorded = UiText.Get("未记录"),
                diagramWaiting = UiText.Get("等待图表代码完整…"), diagramRendering = UiText.Get("正在绘制图表…"),
                diagramFailed = UiText.Get("图表未能绘制，请查看源码。"),
                diagramUnsafe = UiText.Get("图表含不支持的交互或配置，已保留源码。"),
                diagramTooLarge = UiText.Get("图表过于复杂，已保留源码。"),
                diagramUnsupported = UiText.Get("暂不支持此图表类型，已保留源码。"),
                diagramSource = UiText.Get("查看源码"), diagramHideSource = UiText.Get("收起源码"), diagramCopySource = UiText.Get("复制源码"),
                diagramZoomIn = UiText.Get("放大图表"), diagramZoomOut = UiText.Get("缩小图表"), diagramFit = UiText.Get("适应窗口"),
                diagramFullscreen = UiText.Get("全屏查看图表"), diagramClose = UiText.Get("关闭图表"),
                diagramViewport = UiText.Get("Mermaid 图表，可缩放和平移"),
                diagramViewerHint = UiText.Get("拖动平移 · Ctrl＋滚轮缩放 · ＋／－缩放 · 0 适应窗口 · Esc 关闭"),
                reasoning = UiText.Get("思考过程"), thinking = UiText.Get("正在思考…"),
                reasoningDuration = UiText.Get("思考过程 · {0} 秒"), stopped = UiText.Get("已停止生成"),
                interrupted = UiText.Get("回复中断，请重试。"),
                replying = UiText.Get("正在回复…"), generating = UiText.Get("正在生成"),
                toolActivities = UiText.Get("工具活动"), toolRunning = UiText.Get("执行中"),
                toolMoreWebLinks = UiText.Get("另{0}个链接"),
                elapsedSeconds = UiText.Get("用时 {0}秒"), elapsedMinutesSeconds = UiText.Get("用时 {0}分钟{1}秒"),
                elapsedHoursMinutesSeconds = UiText.Get("用时 {0}小时{1}分钟{2}秒"),
                toolCompleted = UiText.Get("已完成"), toolError = UiText.Get("工具失败"),
                toolDenied = UiText.Get("已拒绝"), toolApproval = UiText.Get("等待批准"),
                toolCancelled = UiText.Get("已取消"), toolUnknown = UiText.Get("结果未知"),
                toolSearchWeb = UiText.Get("搜索网页"), toolReadWeb = UiText.Get("阅读网页"), toolReadFile = UiText.Get("读取文件"),
                toolInspectFile = UiText.Get("查看文件"), toolListFiles = UiText.Get("查看文件夹"), toolSearchFiles = UiText.Get("查找文件"),
                toolEditFile = UiText.Get("修改文件"), toolDeleteFile = UiText.Get("删除文件"), toolCreateFolder = UiText.Get("创建文件夹"),
                toolRunCommand = UiText.Get("运行命令"), toolUseSkill = UiText.Get("使用技能"), toolReadSkill = UiText.Get("读取技能"),
                toolFindSkill = UiText.Get("查找技能"), toolInspectSkill = UiText.Get("检查技能"), toolFindTools = UiText.Get("查找工具"),
                toolReadResult = UiText.Get("读取工具记录"), toolFindHistory = UiText.Get("查找聊天记录"), toolReadHistory = UiText.Get("读取聊天记录"),
                toolCompactContext = UiText.Get("整理上下文"),
                toolFindSources = UiText.Get("查找资料"), toolReadSource = UiText.Get("读取资料"),
                toolExecute = UiText.Get("执行操作"), toolOutcomeUncertain = UiText.Get("操作已中断，执行结果尚未确定。"),
                toolTimedOut = UiText.Get("操作超时。"), toolSandboxUnavailable = UiText.Get("沙箱暂不可用。"),
                toolSkillUnavailable = UiText.Get("技能运行环境尚未满足。"), toolApprovalExpired = UiText.Get("批准已过期。"),
                toolCommandUnavailable = UiText.Get("此命令暂不支持。"), toolConnectionUnavailable = UiText.Get("工具连接不可用。"),
                toolAuthRequired = UiText.Get("工具需要认证。"),
                toolInspectWindows = UiText.Get("查看窗口"), toolFindApps = UiText.Get("查找软件"), toolScreenshot = UiText.Get("截图"), toolReadWindow = UiText.Get("读取窗口"),
                toolOpenApp = UiText.Get("打开软件"), toolActivateWindow = UiText.Get("切换窗口"), toolClick = UiText.Get("点击"),
                toolMovePointer = UiText.Get("移动鼠标"), toolScroll = UiText.Get("滚动"), toolDrag = UiText.Get("拖动"),
                toolTypeText = UiText.Get("输入文字"), toolPressKey = UiText.Get("按下按键"),
                toolAdjustWindow = UiText.Get("调整窗口"), toolWindowResize = UiText.Get("调整大小"), toolWindowMaximize = UiText.Get("最大化"),
                toolWindowMinimize = UiText.Get("最小化"), toolWindowRestore = UiText.Get("恢复窗口"), toolBackgroundLaunch = UiText.Get("后台启动"),
                toolWindowUnresponsive = UiText.Get("窗口未响应"),
                toolCharacterCount = UiText.Get("{0}字符"), toolScrollDelta = UiText.Get("滚动量 {0}"), toolViewScreenshot = UiText.Get("查看截图")
            }
        });
    }

    private void SetNotice(string key)
    {
        _noticeKey = key;
        _notice.Text = UiText.Get(key);
    }

    private void MessageChanged(object? sender, PropertyChangedEventArgs e) => QueueRefresh();
    private void QueueRefresh()
    {
        if (_disposed) return;
        _revision++;
        _dirty = true;
        _refresh.Start();
    }

    private async Task InitializeAsync()
    {
        try
        {
            string resources = Path.Combine(AppContext.BaseDirectory, "Resources");
            if (!File.Exists(Path.Combine(resources, "Transcript", "index.html"))) throw new FileNotFoundException(UiText.Get("聊天显示资源缺失。"));
            string cache = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "KYNXA", "Cache", "TranscriptWebView2");
            var environment = await CoreWebView2Environment.CreateWithOptionsAsync(null, cache, null).AsTask().WaitAsync(TimeSpan.FromSeconds(15));
            if (_disposed) return;
            await _browser.EnsureCoreWebView2Async(environment).AsTask().WaitAsync(TimeSpan.FromSeconds(15));
            if (_disposed) return;
            var core = _browser.CoreWebView2;
            core.Settings.AreDevToolsEnabled = false;
            // Keep text editing/selection shortcuts, without browser reload or navigation keys.
            // 保留文本编辑和选择快捷键，同时阻止浏览器刷新与页面导航快捷键。
            core.Settings.AreBrowserAcceleratorKeysEnabled = false;
            core.Settings.IsStatusBarEnabled = false;
            core.Settings.IsWebMessageEnabled = true;
            core.Settings.IsPasswordAutosaveEnabled = false;
            core.Settings.IsGeneralAutofillEnabled = false;
            core.SetVirtualHostNameToFolderMapping(HostName, resources, CoreWebView2HostResourceAccessKind.DenyCors);
            core.NavigationStarting += (_, e) => { if (e.Uri != PageUrl) e.Cancel = true; };
            core.NewWindowRequested += (_, e) => e.Handled = true;
            core.PermissionRequested += (_, e) => e.State = CoreWebView2PermissionState.Deny;
            core.DownloadStarting += (_, e) => e.Cancel = true;
            core.ContextMenuRequested += (_, e) =>
            {
                for (int i = e.MenuItems.Count - 1; i >= 0; i--)
                    if (e.MenuItems[i].Name is not ("copy" or "selectAll")) e.MenuItems.RemoveAt(i);
                if (e.MenuItems.Count == 0) e.Handled = true;
            };
            core.AddWebResourceRequestedFilter("*", CoreWebView2WebResourceContext.All);
            core.WebResourceRequested += (_, e) =>
            {
                if (!IsLocalResource(e.Request.Uri)) e.Response = environment.CreateWebResourceResponse(null, 403, "Forbidden", "");
            };
            core.WebMessageReceived += MessageReceived;
            core.Navigate(PageUrl);
            await _ready.Task.WaitAsync(TimeSpan.FromSeconds(15));
            _notice.Visibility = Visibility.Collapsed;
            QueueRefresh();
        }
        catch (Exception error)
        {
            if (_disposed) return;
            SetNotice("聊天显示未能加载，请重新打开应用。");
            _notice.Visibility = Visibility.Visible;
            _ready.TrySetException(error);
        }
    }

    private async void MessageReceived(CoreWebView2 sender, CoreWebView2WebMessageReceivedEventArgs e)
    {
        if (_disposed || e.Source != PageUrl) return;
        try
        {
            using var document = JsonDocument.Parse(e.WebMessageAsJson);
            var message = document.RootElement;
            if (!message.TryGetProperty("type", out var kind)) return;
            switch (kind.GetString())
            {
                case "ready":
                    _ready.TrySetResult();
                    RefreshAppearance();
                    RefreshLanguage();
                    QueueRefresh();
                    break;
                case "retry":
                    if (MatchesConversation(message) && message.TryGetProperty("id", out var id) && Guid.TryParse(id.GetString(), out var parsed)
                        && _messages.Any(row => row.Message.Id == parsed && row.RetryVisibility == Visibility.Visible)) RetryRequested?.Invoke(this, parsed);
                    break;
                case "link":
                    if (MatchesConversation(message) && message.TryGetProperty("url", out var url) && Uri.TryCreate(url.GetString(), UriKind.Absolute, out var uri)
                        && uri.Scheme is "http" or "https" or "mailto") await Launcher.LaunchUriAsync(uri);
                    break;
                case "toolResult":
                    if (MatchesConversation(message) && _conversationId is { } conversation &&
                        message.TryGetProperty("messageId", out var messageId) && Guid.TryParse(messageId.GetString(), out var assistantId) &&
                        message.TryGetProperty("toolCallId", out var callId) && callId.ValueKind == JsonValueKind.String &&
                        message.TryGetProperty("resultId", out var resultId) && Guid.TryParse(resultId.GetString(), out var resultGuid))
                    {
                        var tool = _messages.FirstOrDefault(row => row.Message.Id == assistantId)?.ToolActivities
                            .FirstOrDefault(item => item.ToolCallId == callId.GetString() && item.ResultRef?.Id == resultGuid);
                        if (tool is not null) ToolResultRequested?.Invoke(this, new(conversation, assistantId, tool));
                    }
                    break;
                case "copy":
                    CopyMessage(message);
                    break;
            }
        }
        catch (Exception error) when (error is JsonException or InvalidOperationException or ArgumentException or System.Runtime.InteropServices.COMException) { }
    }

    private void CopyMessage(JsonElement message)
    {
        if (!MatchesConversation(message) || !message.TryGetProperty("requestId", out var request) || request.ValueKind != JsonValueKind.String
            || string.IsNullOrEmpty(request.GetString()) || request.GetString()!.Length > 80
            || !message.TryGetProperty("messageId", out var messageId) || !Guid.TryParse(messageId.GetString(), out var parsedId)
            || !_messages.Any(row => row.Message.Id == parsedId)
            || !message.TryGetProperty("text", out var text) || text.ValueKind != JsonValueKind.String) return;

        bool succeeded = false;
        try
        {
            var clipboardContent = new DataPackage();
            clipboardContent.SetText(text.GetString() ?? "");
            Clipboard.SetContent(clipboardContent);
            succeeded = true;
        }
        catch (Exception error) when (error is System.Runtime.InteropServices.COMException or ArgumentException or InvalidOperationException) { }

        // A browser click is only a request; acknowledge the actual clipboard result for this chat and operation.
        // 浏览器点击仅代表请求；回执绑定当前聊天与本次操作，反映剪贴板实际结果。
        Post(new { type = "copyResult", conversationId = _conversationId?.ToString() ?? "", requestId = request.GetString(), success = succeeded });
    }

    private bool MatchesConversation(JsonElement message) => message.TryGetProperty("conversationId", out var id)
        && id.GetString() == (_conversationId?.ToString() ?? "");
    private static bool IsLocalResource(string value) => Uri.TryCreate(value, UriKind.Absolute, out var uri) && uri.Scheme == "https" && uri.Host == HostName;
    private void Post(object data)
    {
        if (_disposed || !_ready.Task.IsCompletedSuccessfully) return;
        try { _browser.CoreWebView2.PostWebMessageAsJson(JsonSerializer.Serialize(data)); }
        catch (System.Runtime.InteropServices.COMException) { }
    }

    private async Task FlushAsync()
    {
        if (_disposed || _rendering || !_dirty || !_ready.Task.IsCompletedSuccessfully) return;
        _rendering = true;
        _dirty = false;
        long revision = _revision;
        long generation = _generation;
        var conversationId = _conversationId;
        try
        {
            var snapshots = _messages.Select(row =>
            {
                var visible = TranscriptPresentation.Select(row.Message.Role, row.Message.Status, row.Content,
                    row.Message.AssistantSegments, row.ToolActivities);
                // Public process records belong to the gateway. Hidden phases need no HTML or browser payload.
                // 公开过程记录归网关所有；隐藏的阶段不需要生成 HTML 或传入浏览器。
                var segments = visible.Segments.Select(segment => segment with { Reasoning = "", ReasoningDurationMs = 0 }).ToArray();
                return new Snapshot(row.Message.Id, row.Message.Role, visible.Content, "", row.IsStreaming,
                    visible.Mode == "active" && row.IsThinking ? "thinking" : null, 0,
                    row.IsWaiting, row.Message.Status, row.Message.Error, row.RetryVisibility == Visibility.Visible,
                    visible.Tools, segments, visible.Mode, row.Message.DurationMs,
                    row.IsStreaming && row.Message.GenerationStartedTimestamp is long generationStarted
                        ? Math.Max(0, (long)Stopwatch.GetElapsedTime(generationStarted).TotalMilliseconds) : null,
                    row.Message.CreatedAt > DateTimeOffset.UnixEpoch ? row.Message.CreatedAt.ToUnixTimeMilliseconds() : null,
                    MessageTimePresentation.GetEnd(row.ConversationId, row.Message)?.ToUnixTimeMilliseconds());
            }).ToArray();
            var rendered = new (Snapshot Row, CachedHtml? Cache)[snapshots.Length];
            var previousCaches = new CachedHtml?[snapshots.Length];
            var missing = new List<int>();
            for (int index = 0; index < snapshots.Length; index++)
            {
                var row = snapshots[index];
                _html.TryGetValue(row.Id, out var cached);
                previousCaches[index] = cached;
                bool hit = cached is not null && cached.Role == row.Role && cached.Content == row.Content &&
                    cached.Reasoning == row.Reasoning && cached.Streaming == row.Streaming && cached.Mode == row.Mode &&
                    cached.Segments.Select(segment => segment.Source).SequenceEqual(row.AssistantSegments);
                rendered[index] = (row, hit ? cached : null);
                if (!hit) missing.Add(index);
            }
            // Cached navigation can post immediately. Parse only changed messages off the UI thread.
            // 缓存聊天可立即切换；只在 UI 线程之外解析有变化的消息。
            if (missing.Count > 0) await Task.Run(() =>
            {
                foreach (int index in missing)
                {
                    // Rapid navigation can abandon a large transcript between messages.
                    // 快速切换聊天可能在两条消息之间放弃大段聊天记录的解析。
                    if (generation != Volatile.Read(ref _generation)) return;
                    var row = snapshots[index];
                    // The UI owns _html and may dispose it while this worker is parsing.
                    // Only immutable references captured before the await cross that boundary.
                    // UI 拥有 _html，解析期间可能释放它；跨 await 只使用此前捕获的不可变引用。
                    var previous = previousCaches[index];
                    var segmentHtml = row.AssistantSegments.Select(segment =>
                    {
                        var match = previous?.Segments.FirstOrDefault(value => value.Source == segment);
                        return match ?? new CachedSegmentHtml(segment,
                            TranscriptMarkdown.Render(segment.Content, segment.Status == "streaming"),
                            "");
                    }).ToArray();
                    string html = row.Role == "user" || segmentHtml.Length > 0 ? "" : TranscriptMarkdown.Render(row.Content, row.Streaming);
                    rendered[index] = (row, new CachedHtml(row.Role, row.Content, row.Reasoning, row.Streaming, row.Mode, html, "", segmentHtml));
                }
            });
            if (_disposed || generation != _generation) return;
            // A newer stream tick is allowed to queue behind this snapshot; switching chats is not.
            // 较新的流式刷新可以排在本快照之后；切换到其他聊天则使本快照失效。
            foreach (var item in rendered) CacheHtml(item.Row.Id, item.Cache!);
            Post(new { type = "render", conversationId = conversationId?.ToString() ?? "", revision, openAtBottom = _openAtBottom,
                messages = rendered.Select(item => new { id = item.Row.Id, role = item.Row.Role, content = item.Row.Content,
                    html = item.Cache!.Html, reasoningHtml = item.Cache.ReasoningHtml, reasoningState = item.Row.ReasoningState,
                    reasoningSeconds = item.Row.ReasoningSeconds, status = item.Row.Status,
                    presentationMode = item.Row.Mode, durationMs = item.Row.DurationMs,
                    generationElapsedMs = item.Row.GenerationElapsedMs,
                    createdAtMs = item.Row.CreatedAtMs, endRecordedAtMs = item.Row.EndRecordedAtMs,
                    streaming = item.Row.Streaming, waiting = item.Row.Waiting, error = item.Row.Error, canRetry = item.Row.CanRetry,
                    assistantSegments = item.Cache.Segments.Select(segment => new { id = segment.Source.Id,
                        round = segment.Source.Round, order = segment.Source.Order, phase = segment.Source.Phase,
                        status = segment.Source.Status, content = segment.Source.Content, html = segment.Html,
                        reasoningHtml = segment.ReasoningHtml,
                        reasoningState = segment.Source.Status == "streaming" && item.Row.ReasoningState == "thinking" ? "thinking" : null,
                        reasoningSeconds = 0 }),
                    toolActivities = item.Row.ToolActivities.Select(tool => new { uiRevision = GetToolDisplayRevision(item.Row.Id, tool), toolCallId = tool.ToolCallId, name = tool.Name,
                        arguments = tool.Arguments, status = tool.Status, summary = tool.Summary, result = tool.Result,
                        approvalId = tool.ApprovalId, outsideWorkspace = tool.OutsideWorkspace, sandbox = tool.Sandbox,
                        workspaceRoot = tool.WorkspaceRoot, code = tool.Code, round = tool.Round, order = tool.Order,
                        resultRef = tool.ResultRef is { } reference ? new { id = reference.Id, bytes = reference.Bytes, sha256 = reference.Sha256 } : null }) }) });
            _openAtBottom = false;
            _notice.Visibility = Visibility.Collapsed;
        }
        catch (Exception error) when (error is ArgumentException or InvalidOperationException or System.Text.RegularExpressions.RegexMatchTimeoutException)
        {
            if (_disposed || generation != _generation) return;
            SetNotice("聊天内容暂时无法显示，请重新打开此聊天。");
            _notice.Visibility = Visibility.Visible;
        }
        finally
        {
            _rendering = false;
            if (_dirty && !_disposed) _refresh.Start();
        }
    }

    private long GetToolDisplayRevision(Guid messageId, ToolActivity tool)
    {
        var key = (messageId, tool.ToolCallId);
        if (_toolRevisions.TryGetValue(key, out var previous) && previous.Source.TryGetTarget(out var source) && ReferenceEquals(source, tool))
            return previous.Revision;
        if (!_toolRevisions.ContainsKey(key))
        {
            while (_toolRevisions.Count >= MaximumToolDisplayRevisions) _toolRevisions.Remove(_toolRevisionOrder.Dequeue());
            _toolRevisionOrder.Enqueue(key);
        }
        // Events are immutable records. A weak source avoids retaining full tool results
        // merely to skip unchanged DOM rows during streaming and cached navigation.
        // 事件是不可变记录；弱引用避免仅为跳过未变化的 DOM 行而保留完整工具结果。
        long revision = ++_nextToolRevision;
        _toolRevisions[key] = new(new WeakReference<ToolActivity>(tool), revision);
        return revision;
    }

    private void CacheHtml(Guid id, CachedHtml value)
    {
        if (_html.Remove(id, out var previous)) _cacheCharacters -= previous.Size;
        if (_cacheNodes.Remove(id, out var node)) _cacheOrder.Remove(node);
        if (value.Size > MaximumCacheCharacters) return;
        while (_html.Count > 0 && (_html.Count >= MaximumCachedMessages || _cacheCharacters + value.Size > MaximumCacheCharacters))
        {
            Guid oldest = _cacheOrder.First!.Value;
            _cacheCharacters -= _html[oldest].Size;
            _html.Remove(oldest);
            _cacheNodes.Remove(oldest);
            _cacheOrder.RemoveFirst();
        }
        _html.Add(id, value);
        _cacheNodes.Add(id, _cacheOrder.AddLast(id));
        _cacheCharacters += value.Size;
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        _timeRestoreCancellation?.Cancel();
        _timeRestoreCancellation?.Dispose();
        _generation++;
        _ready.TrySetCanceled();
        ConversationChanged?.Invoke(this, EventArgs.Empty);
        UiText.LanguageChanged -= LanguageChanged;
        AppearanceService.Changed -= AppearanceChanged;
        _refresh.Stop();
        foreach (var message in _messages) message.PropertyChanged -= MessageChanged;
        _messages = [];
        _html.Clear();
        _cacheOrder.Clear();
        _cacheNodes.Clear();
        _toolRevisions.Clear();
        _toolRevisionOrder.Clear();
        _cacheCharacters = 0;
        _browser.Close();
    }
}
