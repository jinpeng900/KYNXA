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

public sealed record ConversationHistoryRequest(Guid ConversationId, string Direction);
public sealed record ConversationAttachmentRequest(Guid ConversationId, Guid MessageId, ChatAttachment Attachment);

/// <summary>
/// A single document keeps browser selection continuous across the whole conversation.
/// 整个聊天使用同一文档，保证浏览器选择可以跨消息连续进行。
/// </summary>
public sealed class ConversationTranscript : Grid, IDisposable
{
    private const string HostName = "kynxa-transcript.local";
    private const string AttachmentHost = "kynxa-attachments.local";
    private const string PageUrl = "https://" + HostName + "/Transcript/index.html";
    private MessageAttachmentsApiClient? _attachmentClient;
    private readonly CancellationTokenSource _attachmentCancellation = new();
    private readonly WebView2 _browser = new() { DefaultBackgroundColor = AppearanceService.ParseColor(AppearanceService.Current.Main) };
    private readonly TextBlock _notice = new() { Text = UiText.Get("正在加载聊天…"), Margin = new Thickness(12), TextWrapping = TextWrapping.Wrap };
    private readonly TaskCompletionSource _ready = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private readonly DispatcherTimer _refresh = new() { Interval = TimeSpan.FromMilliseconds(40) };
    private readonly Dictionary<Guid, CachedHtml> _html = [];
    private readonly Dictionary<Guid, Snapshot> _publishedRows = [];
    private Guid[] _publishedOrder = [];
    private bool _hasPublishedSnapshot;
    private bool _historyHasOlder, _historyHasNewer, _historyHasNewReplies;
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
    private long _diagramExportNavigation;
    private long _findTextRequest;
    private bool _diagramExportPending;
    private bool _openAtBottom, _dirty, _rendering, _disposed;
    private bool _prioritizeNextProjection;
    private sealed record CachedSegmentHtml(AssistantSegment Source, string Html, string ReasoningHtml,
        TranscriptMarkdown.StreamingState? Stream = null)
    {
        public long Size => (long)Source.Content.Length + Source.Reasoning.Length + Html.Length + ReasoningHtml.Length
            + (Stream is null ? 0 : Stream.Source.Length + Stream.StableHtml.Length);
    }
    private sealed record CachedHtml(string Role, string Content, string Reasoning, bool Streaming, string Mode, string Html, string ReasoningHtml,
        CachedSegmentHtml[] Segments, TranscriptMarkdown.StreamingState? Stream = null)
    {
        public long Size => (long)Content.Length + Reasoning.Length + Html.Length + ReasoningHtml.Length + Segments.Sum(segment => segment.Size)
            + (Stream is null ? 0 : Stream.Source.Length + Stream.StableHtml.Length);
    }
    private sealed record Snapshot(Guid Id, string Role, string Content, string Reasoning, bool Streaming,
        string? ReasoningState, long ReasoningSeconds, bool Waiting, string Status, string Error, bool CanRetry,
        ToolActivity[] ToolActivities, AssistantSegment[] AssistantSegments, string Mode, long DurationMs,
        long? GenerationElapsedMs, long? GenerationStartedTimestamp, string GenerationTimingState, Guid GenerationClockId,
        long? CreatedAtMs, long? EndRecordedAtMs, string TaskCompletionState, AttachmentDisplay[] Attachments);
    private sealed record AttachmentDisplay(Guid Id, string Name, string ContentType, long SizeBytes, string Status,
        string? Error, string? ThumbnailUrl);
    private sealed record ToolDisplayRevision(WeakReference<ToolActivity> Source, long Revision);

    public Task Ready => _ready.Task;
    internal WebView2 Browser => _browser;
    internal Func<DiagramExportDocument, Task<DiagramExportOutcome>>? DiagramExportSaveOverride { get; set; }
    public event EventHandler<Guid>? RetryRequested;
    public event EventHandler<ToolResultRequest>? ToolResultRequested;
    public event EventHandler<ConversationAttachmentRequest>? AttachmentRequested;
    public event EventHandler? ConversationChanged;
    public event EventHandler<Guid>? HistoryRequested;
    public event EventHandler<ConversationHistoryRequest>? HistoryNavigationRequested;
    public event EventHandler? ConversationSearchRequested;
    public event EventHandler? FindTextClosed;
    public event EventHandler? FindTextStateChanged;
    public bool IsFindTextOpen { get; private set; }

    public void SetHistoryAvailability(Guid conversationId, bool hasOlder, bool hasNewer, bool hasNewReplies = false)
    {
        if (_conversationId != conversationId) return;
        _historyHasOlder = hasOlder; _historyHasNewer = hasNewer; _historyHasNewReplies = hasNewReplies;
        Post(new { type = "historyAvailability", conversationId = conversationId.ToString(), hasOlder, hasNewer, hasNewReplies });
    }

    public void ForgetConversation(Guid conversationId, IReadOnlyList<Guid> messageIds)
    {
        if (_conversationId == conversationId) return;
        foreach (var id in messageIds)
        {
            if (_html.Remove(id, out var value)) _cacheCharacters -= value.Size;
            if (_cacheNodes.Remove(id, out var node)) _cacheOrder.Remove(node);
        }
        Post(new { type = "forgetConversation", conversationId = conversationId.ToString() });
    }

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
        if (changed)
        {
            _publishedRows.Clear();
            _publishedOrder = [];
            _hasPublishedSnapshot = false;
            _historyHasOlder = _historyHasNewer = _historyHasNewReplies = false;
            _diagramExportNavigation++;
            _findTextRequest++;
            if (IsFindTextOpen)
            {
                IsFindTextOpen = false;
                FindTextStateChanged?.Invoke(this, EventArgs.Empty);
            }
        }
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
        bool prioritize = _prioritizeNextProjection;
        _prioritizeNextProjection = false;
        // A navigation should not wait for the 40 ms stream batching timer.
        // 切换聊天不应等待 40 毫秒的流式合并刷新计时器。
        if (!_rendering && _ready.Task.IsCompletedSuccessfully)
        {
            _refresh.Stop();
            // Render a submitted message before background UI work, without serializing inside the send handler.
            // 发送后的消息优先于后台界面工作投影，同时不在发送事件内同步编码聊天。
            DispatcherQueue.TryEnqueue(prioritize ? Microsoft.UI.Dispatching.DispatcherQueuePriority.Normal
                : Microsoft.UI.Dispatching.DispatcherQueuePriority.Low,
                async () => { if (!_disposed) await FlushAsync(); });
        }
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

    public void BeforeSend()
    {
        _prioritizeNextProjection = true;
        Post(new { type = "beforeSend" });
    }
    public void ClearSelection() => Post(new { type = "clearSelection" });
    internal async Task<bool> IsFollowingLatestAsync(Guid conversationId)
    {
        if (_disposed || _conversationId != conversationId || !_ready.Task.IsCompletedSuccessfully) return false;
        try
        {
            string value = await _browser.ExecuteScriptAsync("window.transcriptState().following && !window.transcriptState().selection");
            return _conversationId == conversationId && value == "true";
        }
        catch (System.Runtime.InteropServices.COMException) { return false; }
    }
    public void JumpToLatest() => Post(new { type = "jumpToLatest" });
    public void OpenFindText()
    {
        if (_disposed) return;
        _ = OpenFindTextAsync(++_findTextRequest, _conversationId);
    }

    private async Task OpenFindTextAsync(long request, Guid? conversationId)
    {
        try
        {
            await _ready.Task;
            // Loading can finish after navigation or a close command; never move focus for an obsolete request.
            // 加载可能晚于会话切换或关闭命令；过期请求不能移动焦点。
            if (_disposed || request != _findTextRequest || conversationId != _conversationId || !_browser.IsLoaded) return;
            _browser.Focus(FocusState.Programmatic);
            // Ready can precede the first batched render. Align identity before opening; an unchanged ID preserves the cached viewport.
            // Ready 可能早于首个合并正文快照；先对齐会话身份再打开查找，相同 ID 不改变已有缓存视口。
            Post(new { type = "openConversation", conversationId = conversationId?.ToString() ?? "" });
            Post(new { type = "findText", action = "open" });
        }
        catch (Exception error)
        {
            // Initialization already exposes its loading failure; keep this focus request independently observed.
            // 初始化会展示加载失败；这里单独观察焦点请求异常，避免产生未观察的任务。
            Debug.WriteLine($"Transcript find open: {error.GetType().Name}");
        }
    }

    public void CloseFindText()
    {
        _findTextRequest++;
        Post(new { type = "findText", action = "close" });
    }

    public void NavigateFindText(bool previous = false)
    {
        // Navigation from native controls keeps their focus and draft; the page enforces selection/modal guards.
        // 原生控件发起导航时保留焦点与草稿；页面继续校验选区和图表模态保护。
        if (!_disposed && IsFindTextOpen) Post(new { type = "findText", action = previous ? "previous" : "next" });
    }

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
        // Appearance changes reach CSS without rebuilding message content or its cached trees.
        // 外观变化只更新 CSS，不重建消息正文或缓存树，字号变化由页面保留阅读锚点。
        Post(new
        {
            type = "setAppearance",
            fontSize = AppearanceService.ChatFontSize,
            palette = new
            {
                main = palette.Main, soft = palette.Soft, accent = palette.Accent, selection = palette.Selection,
                text = palette.Text, secondary = palette.Secondary, border = palette.Border, sidebar = palette.Sidebar,
                muted = palette.Muted, success = palette.Success, error = palette.Error, warning = palette.Warning, replySurface = palette.ReplySurface,
                accentText = palette.AccentText, onAccent = palette.OnAccent,
                codeKeyword = palette.CodeKeyword, codeString = palette.CodeString, codeComment = palette.CodeComment,
                codeNumber = palette.CodeNumber, codeType = palette.CodeType, codeFunction = palette.CodeFunction,
                isDark = palette.IsDark, isHighContrast = palette.IsHighContrast
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
                findText = UiText.Get("查找会话正文"), findTextPlaceholder = UiText.Get("查找当前会话…"),
                findTextPrevious = UiText.Get("上一处"), findTextNext = UiText.Get("下一处"), findTextClose = UiText.Get("关闭正文查找"),
                findTextNoResults = UiText.Get("没有找到匹配内容"), findTextCount = UiText.Get("{0} / {1}"),
                findTextHint = UiText.Get("Enter / ↓ 下一处 · Shift+Enter / ↑ 上一处 · F3 / Shift+F3 切换 · Esc 关闭"),
                findTextSelection = UiText.Get("请先取消文字选择，再跳转到匹配内容。"), findTextSearching = UiText.Get("正在查找…"),
                copy = UiText.Get("复制"), copyMessage = UiText.Get("复制整条消息"), retry = UiText.Get("重试"),
                copied = UiText.Get("已复制"), copyFailed = UiText.Get("复制失败，请重试。"), jumpToLatest = UiText.Get("跳转到最新消息"),
                newReplyNotice = UiText.Get("有新回复，跳转查看"),
                copyBlock = UiText.Get("复制此内容块"), copyCurrentContent = UiText.Get("复制当前内容"), blockPlainText = UiText.Get("文本"),
                blockWrap = UiText.Get("自动换行"), blockUnwrap = UiText.Get("原样显示"),
                blockExpand = UiText.Get("展开"), blockCollapse = UiText.Get("收起"), blockLineCount = UiText.Get("{0} 行"),
                blockClearSelection = UiText.Get("请先取消文字选择，再调整显示。"),
                tableBlockLabel = UiText.Get("表格"), copyTable = UiText.Get("复制表格（可粘贴到 Excel）"),
                copyTableMarkdown = UiText.Get("复制为 Markdown"), tableCopyOptions = UiText.Get("更多复制格式"), tableEmpty = UiText.Get("空表格"),
                messageSentAt = UiText.Get("发送时间"), replyCompletedAt = UiText.Get("回复完成时间"),
                replyEndedAt = UiText.Get("回复结束时间"), timeNotRecorded = UiText.Get("未记录"),
                diagramWaiting = UiText.Get("等待图表代码完整…"), diagramRendering = UiText.Get("正在绘制图表…"),
                diagramFailed = UiText.Get("图表未能绘制，请查看源码。"),
                diagramUnsafe = UiText.Get("图表含不支持的交互或配置，已保留源码。"),
                diagramTooLarge = UiText.Get("图表过于复杂，已保留源码。"),
                diagramUnsupported = UiText.Get("暂不支持此图表类型，已保留源码。"),
                diagramSource = UiText.Get("查看源码"), diagramHideSource = UiText.Get("收起源码"), diagramCopySource = UiText.Get("复制源码"),
                diagramExport = UiText.Get("导出图表"), diagramExportSvg = UiText.Get("导出 SVG"), diagramExportPng = UiText.Get("导出 PNG"),
                diagramExportPreparing = UiText.Get("正在准备图表…"), diagramExporting = UiText.Get("正在保存图表…"),
                diagramExported = UiText.Get("图表已导出"), diagramExportCancelled = UiText.Get("已取消导出"),
                diagramExportFailed = UiText.Get("导出失败，请重试。"), diagramExportReduced = UiText.Get("已按尺寸限制缩放导出"),
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
                taskPartial = UiText.Get("部分完成"), taskVerificationFailed = UiText.Get("核验失败"), taskExecutionUnknown = UiText.Get("执行结果未知"),
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
            string cache = ApplicationPaths.WebViewDirectory;
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
            core.WebResourceRequested += async (_, e) =>
            {
                if (Uri.TryCreate(e.Request.Uri, UriKind.Absolute, out var resource)
                    && resource.Host == AttachmentHost && resource.AbsolutePath.StartsWith("/_attachments/", StringComparison.Ordinal))
                {
                    using var deferral = e.GetDeferral();
                    await ReadAttachmentImageAsync(environment, e, resource);
                }
                else if (!IsLocalResource(e.Request.Uri)) e.Response = environment.CreateWebResourceResponse(null, 403, "Forbidden", "");
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
                    _hasPublishedSnapshot = false;
                    _ready.TrySetResult();
                    RefreshAppearance();
                    RefreshLanguage();
                    QueueRefresh();
                    break;
                case "history":
                    if (MatchesConversation(message) && _conversationId is Guid historyScope)
                    {
                        string? direction = message.TryGetProperty("direction", out var historyDirection) ? historyDirection.GetString() : null;
                        if (direction is "newer" or "latest") HistoryNavigationRequested?.Invoke(this, new(historyScope, direction));
                        else HistoryRequested?.Invoke(this, historyScope);
                    }
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
                case "attachment":
                    if (MatchesConversation(message) && _conversationId is { } attachmentConversation
                        && message.TryGetProperty("messageId", out var attachmentMessage) && Guid.TryParse(attachmentMessage.GetString(), out var attachmentMessageId)
                        && message.TryGetProperty("attachmentId", out var attachmentIdValue) && Guid.TryParse(attachmentIdValue.GetString(), out var attachmentId))
                    {
                        var attachment = _messages.FirstOrDefault(row => row.Message.Id == attachmentMessageId)?.Message.Attachments
                            .FirstOrDefault(item => item.Id == attachmentId);
                        if (attachment is not null && attachment.Status is not ("pending" or "uploading" or "error" or "failed"))
                            AttachmentRequested?.Invoke(this, new(attachmentConversation, attachmentMessageId, attachment));
                    }
                    break;
                case "searchConversations":
                    if (MatchesConversation(message)) ConversationSearchRequested?.Invoke(this, EventArgs.Empty);
                    break;
                case "findTextState":
                    if (MatchesConversation(message) && message.TryGetProperty("open", out var findOpen) &&
                        findOpen.ValueKind is JsonValueKind.True or JsonValueKind.False)
                    {
                        IsFindTextOpen = findOpen.GetBoolean();
                        FindTextStateChanged?.Invoke(this, EventArgs.Empty);
                    }
                    break;
                case "findTextClosed":
                    if (MatchesConversation(message)) FindTextClosed?.Invoke(this, EventArgs.Empty);
                    break;
                case "copy":
                    CopyMessage(message);
                    break;
                case "exportDiagram":
                    await ExportDiagramAsync(message);
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
            // Materialize the package before acknowledging success so closing the app does not erase the copy.
            // 回报成功前将数据实体化到系统剪贴板，避免关闭应用后复制内容随源数据包消失。
            Clipboard.Flush();
            succeeded = true;
        }
        catch (Exception error) when (error is System.Runtime.InteropServices.COMException or ArgumentException or InvalidOperationException) { }

        // A browser click is only a request; acknowledge the actual clipboard result for this chat and operation.
        // 浏览器点击仅代表请求；回执绑定当前聊天与本次操作，反映剪贴板实际结果。
        Post(new { type = "copyResult", conversationId = _conversationId?.ToString() ?? "", requestId = request.GetString(), success = succeeded });
    }

    // Only durable attachments belonging to visible messages can cross the local document boundary.
    // 仅当前聊天已展示消息中的真实附件可以跨越本地文档边界，不开放任意网址或磁盘路径。
    private async Task ReadAttachmentImageAsync(CoreWebView2Environment environment, CoreWebView2WebResourceRequestedEventArgs request, Uri resource)
    {
        try
        {
            string[] parts = resource.AbsolutePath.Split('/', StringSplitOptions.RemoveEmptyEntries);
            if (parts.Length != 4 || !Guid.TryParse(parts[1], out var conversationId) || conversationId != _conversationId
                || !Guid.TryParse(parts[2], out var messageId) || !Guid.TryParse(parts[3], out var attachmentId)
                || _messages.FirstOrDefault(row => row.Message.Id == messageId)?.Message.Attachments
                    .FirstOrDefault(item => item.Id == attachmentId && item.ContentType.StartsWith("image/", StringComparison.OrdinalIgnoreCase)) is null)
            {
                request.Response = environment.CreateWebResourceResponse(null, 403, "Forbidden", "");
                return;
            }
            _attachmentClient ??= new MessageAttachmentsApiClient();
            var image = await _attachmentClient.ReadImageAsync(conversationId, messageId, attachmentId, _attachmentCancellation.Token);
            if (image is null)
            {
                request.Response = environment.CreateWebResourceResponse(null, 404, "Not Found", "");
                return;
            }
            request.Response = environment.CreateWebResourceResponse(new MemoryStream(image.Value.Bytes, writable: false).AsRandomAccessStream(), 200, "OK",
                $"Content-Type: {image.Value.ContentType}\r\nCache-Control: private, max-age=3600\r\nX-Content-Type-Options: nosniff");
        }
        catch (Exception error) when (error is HttpRequestException or OperationCanceledException or System.Runtime.InteropServices.COMException)
        {
            if (!_disposed) request.Response = environment.CreateWebResourceResponse(null, 503, "Unavailable", "");
        }
    }

    private static AttachmentDisplay AttachmentView(Guid conversationId, Guid messageId, ChatAttachment attachment)
    {
        bool image = attachment.ContentType.StartsWith("image/", StringComparison.OrdinalIgnoreCase);
        bool pending = attachment.Status is "pending" or "uploading";
        string? thumbnail = null;
        if (image && attachment.ThumbnailDataUrl is { } preview && IsImageDataUrl(preview)) thumbnail = preview;
        else if (image && !pending && attachment.Status is not ("error" or "failed"))
            thumbnail = $"https://{AttachmentHost}/_attachments/{conversationId:D}/{messageId:D}/{attachment.Id:D}";
        return new(attachment.Id, attachment.Name, attachment.ContentType, attachment.SizeBytes, attachment.Status, attachment.Error, thumbnail);
    }

    private static bool IsImageDataUrl(string value) => value.StartsWith("data:image/png;base64,", StringComparison.Ordinal)
        || value.StartsWith("data:image/jpeg;base64,", StringComparison.Ordinal)
        || value.StartsWith("data:image/webp;base64,", StringComparison.Ordinal)
        || value.StartsWith("data:image/gif;base64,", StringComparison.Ordinal);

    private async Task ExportDiagramAsync(JsonElement message)
    {
        if (!MatchesConversation(message) || _conversationId is not Guid conversationId
            || !message.TryGetProperty("requestId", out var request) || request.ValueKind != JsonValueKind.String
            || string.IsNullOrEmpty(request.GetString()) || request.GetString()!.Length > 80
            || !message.TryGetProperty("messageId", out var messageId) || messageId.ValueKind != JsonValueKind.String
            || !Guid.TryParse(messageId.GetString(), out var parsedId) || !_messages.Any(row => row.Message.Id == parsedId)) return;

        // Snapshot every request field and the owning HWND before the first await; JSON and live UI state are not the document.
        // 首次等待前捕获全部请求字段及所属 HWND；JSON 和可变界面状态不能成为导出文档的数据源。
        string requestId = request.GetString()!;
        string format = message.TryGetProperty("format", out var formatField) && formatField.ValueKind == JsonValueKind.String ? formatField.GetString()! : "";
        string content = message.TryGetProperty("content", out var contentField) && contentField.ValueKind == JsonValueKind.String ? contentField.GetString()! : "";
        long navigation = _diagramExportNavigation;
        bool IsCurrent() => !_disposed && _diagramExportNavigation == navigation && _conversationId == conversationId
            && _messages.Any(row => row.Message.Id == parsedId);
        void Reply(DiagramExportOutcome outcome)
        {
            if (IsCurrent()) Post(new { type = "diagramExportResult", conversationId = conversationId.ToString(), requestId,
                status = outcome switch { DiagramExportOutcome.Success => "success", DiagramExportOutcome.Cancelled => "cancelled", _ => "error" } });
        }
        if (_diagramExportPending) { Reply(DiagramExportOutcome.Error); return; }
        _diagramExportPending = true;
        try
        {
            nint ownerWindow = XamlRoot is null ? 0 : Microsoft.UI.Win32Interop.GetWindowFromWindowId(XamlRoot.ContentIslandEnvironment.AppWindowId);
            var saveOverride = DiagramExportSaveOverride;
            var export = await DiagramExport.ValidateAsync(format, content);
            if (!IsCurrent()) return;
            var outcome = saveOverride is null ? await DiagramExport.SaveAsync(export, ownerWindow) : await saveOverride(export);
            Reply(outcome);
        }
        catch (OperationCanceledException)
        {
            Reply(DiagramExportOutcome.Cancelled);
        }
        catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException or FormatException or ArgumentException
            or InvalidOperationException or System.Xml.XmlException or NotSupportedException or TimeoutException
            or System.Runtime.InteropServices.COMException)
        {
            Reply(DiagramExportOutcome.Error);
        }
        finally { _diagramExportPending = false; }
    }

    private bool MatchesConversation(JsonElement message) => message.TryGetProperty("conversationId", out var id)
        && id.GetString() == (_conversationId?.ToString() ?? "");
    private static bool IsLocalResource(string value) => Uri.TryCreate(value, UriKind.Absolute, out var uri) && uri.Scheme == "https" && uri.Host == HostName;
    private bool Post(object data)
    {
        if (_disposed || !_ready.Task.IsCompletedSuccessfully) return false;
        try { _browser.CoreWebView2.PostWebMessageAsJson(JsonSerializer.Serialize(data)); return true; }
        catch (System.Runtime.InteropServices.COMException) { return false; }
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
            var sourceOrder = _messages.Select(row => row.Message.Id).ToArray();
            bool initialPublished = !_hasPublishedSnapshot && _messages.Count > 4;
            if (initialPublished)
            {
                // Capture and encode the visible tail before projecting earlier task/tool histories.
                // 先捕获并发送可见尾部，再投影更早的任务与工具历史，首屏不等待整页处理。
                var recent = _messages.TakeLast(4).Select(CaptureSnapshot).Select(row =>
                {
                    _html.TryGetValue(row.Id, out var cached);
                    return (Row: row, Cache: MatchesCachedHtml(row, cached) ? cached! : PreviewHtml(row, cached));
                }).ToArray();
                if (!PublishRows(recent, true, conversationId, revision, sourceOrder, _openAtBottom, progressiveSnapshot: true))
                { _dirty = true; return; }
                _publishedRows.Clear(); _publishedOrder = sourceOrder; _hasPublishedSnapshot = true;
                _openAtBottom = false; _notice.Visibility = Visibility.Collapsed;
                await Task.Yield();
                if (_disposed || generation != _generation) return;
            }
            var snapshots = _messages.Select(CaptureSnapshot).ToArray();
            var order = snapshots.Select(row => row.Id).ToArray();
            bool fullSnapshot = !_hasPublishedSnapshot;
            bool orderChanged = !_publishedOrder.SequenceEqual(order);
            // Evicted historical HTML needs no parsing while its published message stays unchanged.
            // 历史 HTML 被淘汰后，只要已发布消息未变化就无需重新解析。
            var changed = fullSnapshot ? snapshots : snapshots.Where(row =>
                !_publishedRows.TryGetValue(row.Id, out var previous) || !SamePublishedSnapshot(previous, row)).ToArray();
            var rendered = new (Snapshot Row, CachedHtml? Cache)[changed.Length];
            var previousCaches = new CachedHtml?[changed.Length];
            var missing = new List<int>();
            for (int index = 0; index < changed.Length; index++)
            {
                var row = changed[index];
                _html.TryGetValue(row.Id, out var cached);
                previousCaches[index] = cached;
                bool hit = MatchesCachedHtml(row, cached);
                rendered[index] = (row, hit ? cached : null);
                if (!hit) missing.Add(index);
            }
            bool progressive = initialPublished || fullSnapshot && changed.Length > 0 || missing.Count > 1;
            if (progressive && !initialPublished)
            {
                // Show cached formatting or escaped source first; Markdown work cannot hold the whole page blank.
                // 先显示缓存排版或安全转义的原文，整页消息不再等待全部 Markdown 解析完成。
                // The first bridge payload contains only recent visible messages, not a second encoded copy of the entire page.
                // 首个跨进程消息只带最近可见消息，不为整页原文再同步编码一份 HTML；其余按批在后台补齐。
                var preview = Enumerable.Range(Math.Max(0, rendered.Length - 4), Math.Min(4, rendered.Length))
                    .Select(index => (rendered[index].Row,
                        Cache: rendered[index].Cache ?? PreviewHtml(rendered[index].Row, previousCaches[index]))).ToArray();
                if (!PublishRows(preview, fullSnapshot, conversationId, revision, orderChanged ? order : null, _openAtBottom,
                    progressiveSnapshot: fullSnapshot))
                { _dirty = true; return; }
                if (fullSnapshot) _publishedRows.Clear();
                foreach (var item in preview) _publishedRows.Remove(item.Row.Id);
                _publishedOrder = order;
                _hasPublishedSnapshot = true;
                _openAtBottom = false;
                _notice.Visibility = Visibility.Collapsed;
                await Task.Yield();
            }
            // Parse recent replies first in bounded batches; all work shares the existing bounded HTML cache.
            // 优先分批解析最近回复，继续共用现有有界 HTML 缓存，不为后台解析另建无界状态。
            var work = progressive ? Enumerable.Range(0, changed.Length) : missing.AsEnumerable();
            foreach (var batch in work.Reverse().Chunk(4))
            {
                await Task.Run(() =>
                {
                    foreach (int index in batch)
                    {
                        if (generation != Volatile.Read(ref _generation)) return;
                        if (rendered[index].Cache is null)
                            rendered[index] = (changed[index], RenderHtml(changed[index], previousCaches[index]));
                    }
                });
                if (_disposed || generation != _generation) return;
                if (!progressive) continue;
                var completed = batch.Select(index => (rendered[index].Row, Cache: rendered[index].Cache!)).ToArray();
                foreach (var item in completed) CacheHtml(item.Row.Id, item.Cache);
                if (!PublishRows(completed, false, conversationId, revision, null, false))
                { _dirty = true; return; }
                foreach (var item in completed) _publishedRows[item.Row.Id] = item.Row;
                // New output takes precedence over the remaining older formatting; unparsed rows remain dirty.
                // 新输出优先于尚未排版的旧消息，未解析行保持待更新，下一次刷新继续补齐。
                if (revision != _revision) { _dirty = true; return; }
            }
            if (_disposed || generation != _generation) return;
            if (!progressive)
            {
                var completed = rendered.Select(item => (item.Row, Cache: item.Cache!)).ToArray();
                foreach (var item in completed) CacheHtml(item.Row.Id, item.Cache);
                if (!PublishRows(completed, fullSnapshot, conversationId, revision, orderChanged ? order : null, _openAtBottom))
                { _dirty = true; return; }
            }
            else
            {
                // Recent-first parsing must still leave the newest messages at the cache's hot end.
                // 解析顺序优先最新消息，但缓存热端仍按消息顺序保留最新内容。
                foreach (var item in rendered) CacheHtml(item.Row.Id, item.Cache!);
            }
            _publishedRows.Clear();
            foreach (var row in snapshots) _publishedRows.Add(row.Id, row);
            _publishedOrder = order;
            _hasPublishedSnapshot = true;
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

    private Snapshot CaptureSnapshot(ConversationMessageViewModel row)
    {
        var visible = TranscriptPresentation.Select(row.Message.Role, row.Message.Status, row.Content,
            row.Message.AssistantSegments, row.ToolActivities, row.Message.TaskCompletionState);
        // Public process records belong to the gateway. Hidden phases need no HTML or browser payload.
        // 公开过程记录归网关所有；隐藏的阶段不需要生成 HTML 或传入浏览器。
        var segments = visible.Segments.Select(segment => segment with { Reasoning = "", ReasoningDurationMs = 0 }).ToArray();
        return new Snapshot(row.Message.Id, row.Message.Role, visible.Content, "", row.IsStreaming,
            visible.Mode == "active" && row.IsThinking ? "thinking" : null, 0,
            row.IsWaiting, row.Message.Status, row.Message.Error, row.RetryVisibility == Visibility.Visible,
            visible.Tools, segments, visible.Mode, row.Message.DurationMs,
            row.Message.GenerationElapsedMs, row.Message.GenerationStartedTimestamp,
            row.Message.GenerationTimingState, row.Message.GenerationClockId,
            row.Message.CreatedAt > DateTimeOffset.UnixEpoch ? row.Message.CreatedAt.ToUnixTimeMilliseconds() : null,
            MessageTimePresentation.GetEnd(row.ConversationId, row.Message)?.ToUnixTimeMilliseconds(), row.Message.TaskCompletionState,
            row.Message.Attachments.Select(attachment => AttachmentView(row.ConversationId, row.Message.Id, attachment)).ToArray());
    }

    private static bool MatchesCachedHtml(Snapshot row, CachedHtml? cached) =>
        cached is not null && cached.Role == row.Role && cached.Content == row.Content &&
        cached.Reasoning == row.Reasoning && cached.Streaming == row.Streaming && cached.Mode == row.Mode &&
        cached.Segments.Select(segment => segment.Source).SequenceEqual(row.AssistantSegments);

    private static CachedHtml PreviewHtml(Snapshot row, CachedHtml? previous)
    {
        static string PlainHtml(string content) => content.Length == 0 ? ""
            : "<div class=\"markdown-pending\" style=\"white-space:pre-wrap\">"
                + System.Net.WebUtility.HtmlEncode(content.Length > 8192 ? content[^8192..] : content) + "</div>";
        var segments = row.AssistantSegments.Select(segment =>
            previous?.Segments.FirstOrDefault(value => value.Source == segment)
                ?? new CachedSegmentHtml(segment, PlainHtml(segment.Content), "")).ToArray();
        return new(row.Role, row.Content, row.Reasoning, row.Streaming, row.Mode,
            row.Role != "user" && segments.Length == 0 ? PlainHtml(row.Content) : "", "", segments);
    }

    private static CachedHtml RenderHtml(Snapshot row, CachedHtml? previous)
    {
        var segments = row.AssistantSegments.Select(segment =>
        {
            var match = previous?.Segments.FirstOrDefault(value => value.Source == segment);
            if (match is not null) return match;
            if (segment.Status == "streaming")
            {
                var prior = previous?.Segments.FirstOrDefault(value => value.Source.Id == segment.Id);
                var stream = TranscriptMarkdown.RenderStreaming(segment.Content, prior?.Stream);
                return new CachedSegmentHtml(segment, stream.Html, "", stream.State);
            }
            return new CachedSegmentHtml(segment, TranscriptMarkdown.Render(segment.Content), "");
        }).ToArray();
        string html = "";
        TranscriptMarkdown.StreamingState? state = null;
        if (row.Role != "user" && segments.Length == 0)
        {
            if (row.Streaming)
            {
                var stream = TranscriptMarkdown.RenderStreaming(row.Content, previous?.Stream);
                html = stream.Html; state = stream.State;
            }
            else html = TranscriptMarkdown.Render(row.Content);
        }
        return new(row.Role, row.Content, row.Reasoning, row.Streaming, row.Mode, html, "", segments, state);
    }

    private bool PublishRows(IReadOnlyList<(Snapshot Row, CachedHtml Cache)> rendered, bool fullSnapshot,
        Guid? conversationId, long revision, Guid[]? messageOrder, bool openAtBottom, bool progressiveSnapshot = false)
    {
        return Post(new { type = fullSnapshot ? "render" : "renderPatch", conversationId = conversationId?.ToString() ?? "", revision, openAtBottom, progressiveSnapshot,
            messageOrder,
            historyHasOlder = _historyHasOlder, historyHasNewer = _historyHasNewer, historyHasNewReplies = _historyHasNewReplies,
            messages = rendered.Select(item => new { id = item.Row.Id, role = item.Row.Role, content = item.Row.Content,
                html = item.Cache.Html, reasoningHtml = item.Cache.ReasoningHtml, reasoningState = item.Row.ReasoningState,
                htmlStableLength = item.Cache.Stream?.StableHtml.Length, htmlStableBlocks = item.Cache.Stream?.StableBlocks,
                reasoningSeconds = item.Row.ReasoningSeconds, status = item.Row.Status,
                presentationMode = item.Row.Mode, durationMs = item.Row.DurationMs,
                taskCompletionState = item.Row.TaskCompletionState,
                attachments = item.Row.Attachments.Select(attachment => new { id = attachment.Id, name = attachment.Name,
                    contentType = attachment.ContentType, sizeBytes = attachment.SizeBytes, status = attachment.Status,
                    error = attachment.Error, thumbnailUrl = attachment.ThumbnailUrl }),
                generationElapsedMs = item.Row.GenerationElapsedMs,
                generationTimingState = item.Row.GenerationTimingState, generationClockId = item.Row.GenerationClockId,
                createdAtMs = item.Row.CreatedAtMs, endRecordedAtMs = item.Row.EndRecordedAtMs,
                streaming = item.Row.Streaming, waiting = item.Row.Waiting, error = item.Row.Error, canRetry = item.Row.CanRetry,
                assistantSegments = item.Cache.Segments.Select(segment => new { id = segment.Source.Id,
                    round = segment.Source.Round, order = segment.Source.Order, phase = segment.Source.Phase,
                    isAnswerContinuation = segment.Source.IsAnswerContinuation,
                    status = segment.Source.Status, content = segment.Source.Content, html = segment.Html,
                    htmlStableLength = segment.Stream?.StableHtml.Length, htmlStableBlocks = segment.Stream?.StableBlocks,
                    reasoningHtml = segment.ReasoningHtml,
                    reasoningState = segment.Source.Status == "streaming" && item.Row.ReasoningState == "thinking" ? "thinking" : null,
                    reasoningSeconds = 0 }),
                toolActivities = item.Row.ToolActivities.Select(tool => new { uiRevision = GetToolDisplayRevision(item.Row.Id, tool), toolCallId = tool.ToolCallId, name = tool.Name,
                    arguments = tool.Arguments, status = tool.Status, summary = tool.Summary, result = tool.Result,
                    approvalId = tool.ApprovalId, outsideWorkspace = tool.OutsideWorkspace, sandbox = tool.Sandbox,
                    workspaceRoot = tool.WorkspaceRoot, code = tool.Code, round = tool.Round, order = tool.Order,
                    resultRef = tool.ResultRef is { } reference ? new { id = reference.Id, bytes = reference.Bytes, sha256 = reference.Sha256 } : null }) }) });
    }

    // Compare the stable origin, not every elapsed millisecond; inherited clocks still need one metadata update.
    // 比较稳定起点而非每毫秒变化；继承更早计时时仅发布一次元数据，不增加逐帧更新。
    private static bool SamePublishedSnapshot(Snapshot previous, Snapshot current) =>
        previous.Id == current.Id && previous.Role == current.Role && previous.Content == current.Content &&
        previous.Reasoning == current.Reasoning && previous.Streaming == current.Streaming &&
        previous.ReasoningState == current.ReasoningState && previous.ReasoningSeconds == current.ReasoningSeconds &&
        previous.Waiting == current.Waiting && previous.Status == current.Status && previous.Error == current.Error &&
        previous.CanRetry == current.CanRetry && previous.Mode == current.Mode && previous.DurationMs == current.DurationMs &&
        previous.GenerationStartedTimestamp == current.GenerationStartedTimestamp &&
        previous.GenerationTimingState == current.GenerationTimingState && previous.GenerationClockId == current.GenerationClockId &&
        (current.GenerationTimingState == "live" || previous.GenerationElapsedMs == current.GenerationElapsedMs) &&
        previous.CreatedAtMs == current.CreatedAtMs && previous.EndRecordedAtMs == current.EndRecordedAtMs &&
        previous.TaskCompletionState == current.TaskCompletionState && previous.Attachments.SequenceEqual(current.Attachments) &&
        previous.AssistantSegments.SequenceEqual(current.AssistantSegments) && previous.ToolActivities.SequenceEqual(current.ToolActivities);

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
        _publishedRows.Clear();
        _publishedOrder = [];
        _cacheOrder.Clear();
        _cacheNodes.Clear();
        _toolRevisions.Clear();
        _toolRevisionOrder.Clear();
        _cacheCharacters = 0;
        _attachmentCancellation.Cancel();
        _attachmentCancellation.Dispose();
        _attachmentClient?.Dispose();
        _browser.Close();
    }
}
