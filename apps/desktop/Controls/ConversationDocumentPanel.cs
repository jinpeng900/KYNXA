using System.Diagnostics;
using System.Net;
using System.Text;
using KYNXA.Contracts;
using System.Text.Json;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.Web.WebView2.Core;
using Windows.ApplicationModel.DataTransfer;
using Windows.System;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Open tabs retain bounded views and parsed projections; inactive views yield browser resources.
/// 已打开标签保留有界视图和解析投影；闲置视图让出浏览器资源，切换不重复解析文档。
/// </summary>
public sealed class ConversationDocumentPanel : Grid, IDisposable
{
    private const string PreviewHost = "kynxa-workspace.local";
    private const string PreviewPage = "https://" + PreviewHost + "/Workspace/index.html";
    private const string DocumentContentHost = "kynxa-workspace-content.local";
    private static Task<CoreWebView2Environment>? _previewEnvironment;
    private static Task<CoreWebView2Environment>? _websiteEnvironment;
    private readonly WorkspacePreviewApiClient _api = new();
    private readonly MessageAttachmentsApiClient _attachments = new();
    private readonly Dictionary<string, CachedDocument> _documents = new(StringComparer.Ordinal);
    private readonly Dictionary<string, BrowserView> _views = new(StringComparer.Ordinal);
    private readonly List<Task> _closingChrome = [];
    private static readonly int MaximumRetainedViews = GC.GetGCMemoryInfo().TotalAvailableMemoryBytes >= 24L * 1024 * 1024 * 1024 ? 12 :
        GC.GetGCMemoryInfo().TotalAvailableMemoryBytes >= 12L * 1024 * 1024 * 1024 ? 8 : 4;
    private const long MaximumCachedTextCharacters = 32L * 1024 * 1024;
    private long _viewClock;
    private readonly Dictionary<string, DocumentWork> _documentWork = new(StringComparer.Ordinal);
    private readonly List<DocumentWork> _pendingDocuments = [];
    private int _preparingDocuments;
    private sealed record CachedDocument(WorkspacePreviewTarget Preview, bool ShowSource, string? RequestedVersion);
    // Each open resource owns preparation; selection only controls which completed view is presented.
    // 每份资源独立持有准备过程，标签选择只决定展示哪个视图，不取消其他资源。
    private sealed class DocumentWork(string key, Guid conversationId, string path, string? title, bool userSelected,
        string? requestedVersion, bool changes, Guid? messageId, ChatAttachment? attachment, Guid? mountId)
    {
        public string Key { get; } = key;
        public Guid ConversationId { get; } = conversationId;
        public string Path { get; } = path;
        public string? Title { get; } = title;
        public bool UserSelected { get; } = userSelected;
        public string? RequestedVersion { get; } = requestedVersion;
        public bool IsChanges { get; } = changes;
        public Guid? MessageId { get; } = messageId;
        public ChatAttachment? Attachment { get; } = attachment;
        public Guid? MountId { get; } = mountId;
        public CancellationTokenSource Cancellation { get; } = new();
        public TaskCompletionSource Completion { get; set; } = NewReady();
        public WorkspacePreviewTarget? Preview { get; set; }
        public Uri? ContentAddress { get; set; }
        public BrowserView? View { get; set; }
        public bool ShowSource { get; set; }
        public bool Queued { get; set; }
        public bool Preparing { get; set; }
        public bool Closed { get; set; }
        public string? Error { get; set; }
        public Task? PdfFallback { get; set; }
        public bool PdfFallbackQueued { get; set; }
    }
    private sealed class BrowserView(string key, bool website)
    {
        public string Key { get; } = key;
        public bool IsWebsite { get; } = website;
        public bool Active { get; set; }
        public WebView2 Browser { get; } = new() { DefaultBackgroundColor = AppearanceService.ParseColor(AppearanceService.Current.Main) };
        public TaskCompletionSource Ready { get; set; } = NewReady();
        public Task? Initialization { get; set; }
        public long LastUsed { get; set; }
        public string? RenderedSignature { get; set; }
        public Uri? ContentAddress { get; set; }
        public bool Preparing { get; set; }
        public bool BackgroundPreparing { get; set; }
        public DocumentWork? Work { get; set; }
        public TaskCompletionSource Rendered { get; set; } = NewReady();
        public long RenderId { get; set; }
        public TaskCompletionSource Navigated { get; set; } = NewReady();
        public bool Closed { get; set; }
    }
    private sealed class DirectDocumentPreviewException : Exception
    {
        public DirectDocumentPreviewException() : base("Office direct rendering needs the page converter.") { }
    }
    private readonly Grid _viewport = new();
    private readonly Grid _toolbar = new() { ColumnSpacing = 4, Padding = new Thickness(6, 4, 6, 4) };
    private readonly Button _back = QuietIcon("\uE72B", "后退");
    private readonly Button _forward = QuietIcon("\uE72A", "前进");
    private readonly Button _reload = QuietIcon("\uE72C", "刷新预览");
    private readonly Button _source = QuietButton("</>", "查看源码");
    private readonly Button _more = QuietButton("继续加载", "继续加载");
    private readonly Button _browserOptions = QuietIcon("\uE712", "网站数据");
    private readonly Button _openWebsite = QuietButton("打开网页", "打开网页");
    private readonly TextBox _address = new() { MinWidth = 0, MinHeight = 28, CornerRadius = new CornerRadius(7),
        Padding = new Thickness(8, 3, 8, 3), FontSize = 12, PlaceholderText = "https://" };
    private readonly TextBlock _notice = new() { Margin = new Thickness(10, 4, 10, 4), FontSize = 12,
        TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed };
    private readonly Button _alternativeSearch = QuietButton("切换搜索源", "切换搜索源");
    private Uri? _alternativeSearchAddress;
    private bool _showingVerification;
    private readonly TextBlock _loading = new() { Text = UiText.Get("正在打开文件…"), Margin = new Thickness(16),
        HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center,
        TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed };
    private readonly TaskCompletionSource _loaded = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private readonly Dictionary<string, string> _viewStates = new(StringComparer.Ordinal);
    private readonly Dictionary<string, Uri> _navigationUris = new(StringComparer.Ordinal);
    private readonly Queue<string> _viewOrder = new();
    private sealed record ChromeTab(ConversationChromeView View, Task Initialization) { public long Touched { get; set; } }
    private readonly Dictionary<string, ChromeTab> _chromeViews = new(StringComparer.Ordinal);
    private ConversationChromeView? _chrome;
    private long _chromeSequence;
    private WebView2? _document, _website;
    private WorkspacePreviewTarget? _current;
    private Uri? _contentAddress;
    private Uri? _websiteAddress;
    private Guid _conversationId;
    private string? _activeKey, _path, _title, _requestedVersion, _websiteKey;
    private bool _userSelected, _isWebsite, _isChanges, _showSource, _enabled = true, _disposed;
    private bool _websiteActivationRequested, _clearingWebsiteData;
    private Guid? _attachmentMessageId;
    private Guid? _mountId;
    private ChatAttachment? _attachment;
    private long _generation;

    public event EventHandler<string>? WebsiteRequested;
    internal WebView2? DocumentBrowser => _document;
    internal WebView2? WebsiteBrowser => _website;
    internal ConversationChromeView? ChromeBrowser => _chrome;
    internal WebView2? PreparedResourceBrowser(Guid conversationId, string path) =>
        _views.TryGetValue($"{conversationId:D}:file:{path}", out var view) && view.RenderedSignature is not null
            && !view.Preparing && !view.Closed ? view.Browser : null;
    internal bool IsResourcePreparingInBackground(Guid conversationId, string path) =>
        _views.TryGetValue($"{conversationId:D}:file:{path}", out var view) && !view.Closed && view.BackgroundPreparing;

    public ConversationDocumentPanel()
    {
        RowDefinitions.Add(new() { Height = GridLength.Auto });
        RowDefinitions.Add(new() { Height = GridLength.Auto });
        RowDefinitions.Add(new() { Height = new GridLength(1, GridUnitType.Star) });
        _toolbar.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        _toolbar.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        _toolbar.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        _toolbar.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        _toolbar.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        _toolbar.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        AddToolbar(_back, 0); AddToolbar(_forward, 1); AddToolbar(_reload, 2);
        AddToolbar(_address, 3); AddToolbar(_source, 4); AddToolbar(_more, 5);
        _toolbar.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        AddToolbar(_browserOptions, 6);
        _viewport.Children.Add(_loading);
        _openWebsite.HorizontalAlignment = HorizontalAlignment.Center;
        _openWebsite.VerticalAlignment = VerticalAlignment.Center;
        _openWebsite.Visibility = Visibility.Collapsed;
        _openWebsite.Click += (_, _) => ActivateWebsite();
        _viewport.Children.Add(_openWebsite);
        var noticeRow = new Grid();
        noticeRow.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        noticeRow.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        noticeRow.Children.Add(_notice); SetColumn(_alternativeSearch, 1); noticeRow.Children.Add(_alternativeSearch);
        _alternativeSearch.Visibility = Visibility.Collapsed;
        _alternativeSearch.Margin = new Thickness(2, 3, 6, 3);
        _alternativeSearch.Click += async (_, _) =>
        {
            if (_alternativeSearchAddress is { } address) await NavigateChromeAsync("navigate", address);
        };
        Children.Add(_toolbar); SetRow(noticeRow, 1); Children.Add(noticeRow); SetRow(_viewport, 2); Children.Add(_viewport);
        _back.Click += async (_, _) => await NavigateChromeAsync("back");
        _forward.Click += async (_, _) => await NavigateChromeAsync("forward");
        _reload.Click += (_, _) => RefreshCurrent();
        _source.Click += async (_, _) => await ToggleSourceAsync();
        _more.Click += async (_, _) => await LoadMoreAsync();
        _address.KeyDown += AddressKeyDown;
        _browserOptions.Click += (_, _) => ShowWebsiteDataMenu();
        AutomationProperties.SetName(_address, UiText.Get("网页地址"));
        Loaded += (_, _) =>
        {
            _loaded.TrySetResult();
            DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () => _ = WarmPreviewEnvironmentAsync());
        };
        AppearanceService.Changed += AppearanceChanged;
        UiText.LanguageChanged += LanguageChanged;
        SetToolbarMode(false); ApplyAppearance();
    }

    public void ShowResource(Guid conversationId, string path, string? title = null, bool userSelected = false, string? version = null, Guid? mountId = null)
    {
        if (_disposed || conversationId == Guid.Empty || string.IsNullOrWhiteSpace(path)) return;
        string key = $"{conversationId:D}:file:{path}";
        if (_activeKey == key && !_isWebsite && _requestedVersion == version) return;
        SaveViewState(); RememberDocument();
        _isChanges = false; _attachment = null; _attachmentMessageId = null; _mountId = mountId;
        _conversationId = conversationId; _path = path; _title = title; _userSelected = userSelected;
        _requestedVersion = version; _activeKey = key; _isWebsite = false; _showSource = false; _current = null;
        _contentAddress = null;
        OpenCachedOrRead();
    }

    public void ShowAttachment(Guid conversationId, Guid messageId, ChatAttachment attachment)
    {
        if (_disposed || conversationId == Guid.Empty || messageId == Guid.Empty) return;
        string key = $"{conversationId:D}:attachment:{messageId:D}/{attachment.Id:D}";
        if (_activeKey == key && _requestedVersion == attachment.Sha256) return;
        SaveViewState(); RememberDocument();
        _conversationId = conversationId; _attachmentMessageId = messageId; _attachment = attachment; _mountId = null;
        _isChanges = false; _isWebsite = false; _path = attachment.Name; _title = attachment.Name;
        _requestedVersion = attachment.Sha256; _activeKey = key; _showSource = false; _current = null; _contentAddress = null;
        OpenCachedOrRead();
    }

    public void ShowChanges(Guid conversationId, Guid? mountId = null)
    {
        if (_disposed || conversationId == Guid.Empty) return;
        string key = $"{conversationId:D}:changes:{mountId:D}";
        if (_activeKey == key) return;
        SaveViewState(); RememberDocument();
        _conversationId = conversationId; _activeKey = key; _isChanges = true; _isWebsite = false; _mountId = mountId;
        _attachment = null; _attachmentMessageId = null; _path = "changes.diff"; _title = UiText.Get("工作区变更");
        _requestedVersion = null; _showSource = false; _current = null; _contentAddress = null;
        OpenCachedOrRead();
    }

    private void OpenCachedOrRead()
    {
        if (_activeKey is not { } key || _path is null) return;
        ++_generation;
        DocumentWork? work = _documentWork.GetValueOrDefault(key);
        if (work is not null && work.RequestedVersion != _requestedVersion)
        {
            CloseDocumentWork(work); _documentWork.Remove(key); work = null;
            if (_views.TryGetValue(key, out var existing)) existing.RenderedSignature = null;
        }
        if (work is null)
        {
            work = new(key, _conversationId, _path, _title, _userSelected, _requestedVersion, _isChanges, _attachmentMessageId, _attachment, _mountId);
            if (_documents.TryGetValue(key, out var cached)
                && (_requestedVersion is null || cached.RequestedVersion == _requestedVersion || cached.Preview.Version == _requestedVersion))
            {
                work.Preview = cached.Preview; work.ShowSource = cached.ShowSource;
                work.ContentAddress = ContentAddress(work, cached.Preview.ContentUrl);
            }
            _documentWork[key] = work;
        }
        PresentDocument(work);
        if (_enabled && work.Error is null && (work.Preview is null || work.View is null || work.View.Closed)) QueueDocument(work);
    }

    private Uri? ContentAddress(DocumentWork work, string? value)
    {
        if (value is null) return null;
        if (work.Attachment is null) return _api.ContentAddress(value);
        var address = new Uri(ModelGatewayService.Address, value);
        string? version = work.Attachment.Sha256;
        if (string.IsNullOrEmpty(version) && work.Preview?.Version is { Length: 64 } acceptedVersion)
            version = acceptedVersion;
        // Keep the immutable version returned by the attachment client inside the exact scoped address.
        // 附件客户端返回的不可变版本属于合法地址；仍精确限制同源、消息与附件路径，不能放行任意查询。
        string query = $"messageId={work.MessageId:D}";
        if (!string.IsNullOrEmpty(version)) query += "&version=" + Uri.EscapeDataString(version);
        if (work.Preview?.Kind == "pdf" && !string.Equals(Path.GetExtension(work.Attachment.Name), ".pdf", StringComparison.OrdinalIgnoreCase))
            query += "&representation=pdf";
        var expected = new Uri(ModelGatewayService.Address,
            $"/api/conversations/{work.ConversationId:D}/attachments/{work.Attachment.Id:D}/content?{query}");
        if (address != expected) throw new InvalidDataException(UiText.Get("文件预览地址无效。"));
        return address;
    }

    private void RememberDocument()
    {
        if (_activeKey is { } key && _documentWork.TryGetValue(key, out var work) && _current is not null)
        {
            work.Preview = _current; work.ShowSource = _showSource;
            CacheDocument(work);
        }
    }

    private void CacheDocument(DocumentWork work)
    {
        if (work.Closed || work.Preview is null) return;
        _documents.Remove(work.Key); _documents[work.Key] = new(work.Preview, work.ShowSource, work.RequestedVersion);
        while (_documents.Count > 64 || _documents.Values.Sum(item => (long)(item.Preview.Text?.Length ?? 0)) > MaximumCachedTextCharacters)
        {
            string? oldest = _documents.Keys.FirstOrDefault(key => key != _activeKey
                && (!_documentWork.TryGetValue(key, out var pending) || !pending.Preparing && !pending.Queued));
            if (oldest is null) break;
            _documents.Remove(oldest);
            if (_documentWork.Remove(oldest, out var removed)) CloseDocumentWork(removed);
        }
    }

    private bool IsSelected(DocumentWork work) => !_disposed && !work.Closed && _enabled && !_isWebsite && _activeKey == work.Key;
    private bool IsLive(DocumentWork work) => !_disposed && !work.Closed && !work.Cancellation.IsCancellationRequested
        && _documentWork.GetValueOrDefault(work.Key) == work;

    private void PresentDocument(DocumentWork work)
    {
        if (!IsSelected(work)) return;
        _current = work.Preview; _showSource = work.ShowSource; _contentAddress = work.ContentAddress;
        SetToolbarMode(false); SetNotice(work.Error ?? work.Preview?.Notice);
        if (work.View is { Closed: false } view)
        {
            if (view.IsWebsite) { _website = view.Browser; _websiteKey = work.Key; }
            else _document = view.Browser;
            ActivateView(view.Browser);
            if (view.RenderedSignature is not null) Reveal(view.Browser);
        }
        else HideOldContent();
        if (work.Preview is { } preview) SetPreviewActions(preview);
        Busy(work.Error is null && (work.Preview is null || work.Preparing || work.Queued), "正在打开文件…");
    }

    private static int PreparationConcurrency()
    {
        if (int.TryParse(Environment.GetEnvironmentVariable("KYNXA_DOCUMENT_PREVIEW_CONCURRENCY"), out int configured))
            return Math.Clamp(configured, 1, 32);
        long available = GC.GetGCMemoryInfo().TotalAvailableMemoryBytes;
        int memorySlots = available > 0 ? (int)Math.Clamp(available / (3L * 1024 * 1024 * 1024), 2, 12) : 4;
        return Math.Min(Math.Clamp(Environment.ProcessorCount / 2, 2, 12), memorySlots);
    }

    private Task QueueDocument(DocumentWork work)
    {
        if (work.Closed) return Task.CompletedTask;
        if (!work.Preparing && !work.Queued)
        {
            work.Completion = NewReady(); work.Queued = true; work.Error = null;
            _pendingDocuments.Add(work);
        }
        DispatchDocuments();
        return work.Completion.Task;
    }

    private void DispatchDocuments()
    {
        // Hardware-scaled slots bound background work while the selected queued document goes first.
        // 按硬件分配后台额度，当前选中的排队文档优先，不让切换反复触发读取。
        int concurrency = PreparationConcurrency();
        while (!_disposed && _preparingDocuments < concurrency && _pendingDocuments.Count > 0)
        {
            var work = _pendingDocuments.FirstOrDefault(item => item.Key == _activeKey) ?? _pendingDocuments[0];
            int background = _views.Values.Count(view => !view.Closed && !view.Preparing && view.BackgroundPreparing);
            // Bound automatic background admission, while reserving one preparation slot for the selected document.
            // 自动后台准入受硬件额度限制，同时为当前选中文档保留一个准备名额。
            if (_preparingDocuments + background >= concurrency
                && (work.Key != _activeKey || _preparingDocuments > 0)) break;
            _pendingDocuments.Remove(work); work.Queued = false;
            if (!IsLive(work)) { work.Completion.TrySetResult(); continue; }
            work.Preparing = true; _preparingDocuments++;
            _ = PrepareDocumentAsync(work);
        }
    }

    public void ShowWebsite(Guid conversationId, string url, string? title = null, bool activate = true)
    {
        if (_disposed || conversationId == Guid.Empty || !TryWebsite(url, out var address)) return;
        string key = $"{conversationId:D}:web:{address.AbsoluteUri}";
        if (_activeKey == key && _isWebsite)
        {
            if (activate && !_websiteActivationRequested) ActivateWebsite();
            return;
        }
        SaveViewState(); RememberDocument();
        _isChanges = false; _attachment = null; _attachmentMessageId = null;
        _conversationId = conversationId; _activeKey = key; _isWebsite = true; _title = title;
        _current = null; _contentAddress = null;
        _websiteAddress = _navigationUris.TryGetValue(key, out var savedAddress) ? savedAddress : address;
        _websiteActivationRequested = activate || _chromeViews.ContainsKey(key);
        long generation = ++_generation;
        if (_websiteActivationRequested) _ = OpenWebsiteAsync(_websiteAddress, generation);
        else
        {
            HideOldContent(); _chrome = null; SetToolbarMode(true); _address.Text = _websiteAddress.AbsoluteUri;
            SetNotice(null); Busy(false); _openWebsite.Visibility = Visibility.Visible;
        }
    }

    private void ActivateWebsite()
    {
        if (_disposed || _clearingWebsiteData || !_isWebsite || _websiteAddress is null) return;
        _websiteActivationRequested = true;
        _openWebsite.Visibility = Visibility.Collapsed;
        if (_enabled) _ = OpenWebsiteAsync(_websiteAddress, ++_generation);
    }

    public void SetPreviewEnabled(bool enabled)
    {
        if (_disposed) return;
        if (_enabled == enabled) return;
        if (!enabled) SaveViewState();
        _enabled = enabled;
        if (!enabled)
        {
            ++_generation;
            foreach (var tab in _chromeViews.Values) tab.View.SetActive(false);
            foreach (var view in _views.Values) DeactivateView(view);
            if (_website?.CoreWebView2 is { } browser) browser.MemoryUsageTargetLevel = CoreWebView2MemoryUsageTargetLevel.Low;
        }
        else
        {
            if (_isWebsite)
            {
                if (_websiteActivationRequested && _websiteAddress is { } address) _ = OpenWebsiteAsync(address, ++_generation);
            }
            else if (_path is not null) OpenCachedOrRead();
        }
    }

    private void BeginRead()
    {
        if (_activeKey is not { } key) return;
        if (_documentWork.Remove(key, out var previous)) CloseDocumentWork(previous);
        _documents.Remove(key);
        if (_views.TryGetValue(key, out var view)) view.RenderedSignature = null;
        _current = null;
        OpenCachedOrRead();
    }

    private async Task PrepareDocumentAsync(DocumentWork work)
    {
        var token = work.Cancellation.Token;
        try
        {
            if (work.Preview is null)
            {
                work.Preview = work.Attachment is { } attachment && work.MessageId is { } messageId
                    ? await _attachments.ReadPreviewAsync(work.ConversationId, messageId, attachment, cancellationToken: token)
                    : work.IsChanges ? await _api.ReadChangesAsync(work.ConversationId, token, work.MountId)
                    : await _api.ReadAsync(work.ConversationId, work.Path, userSelected: work.UserSelected, cancellationToken: token, mountId: work.MountId);
                if (!IsLive(work)) return;
                work.ContentAddress = ContentAddress(work, work.Preview.ContentUrl);
                CacheDocument(work);
            }
            await RenderDocumentAsync(work, token);
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested) { }
        catch (Exception error) { if (IsLive(work)) { work.Error = error.Message; if (IsSelected(work)) Fail(error); } }
        finally
        {
            work.Preparing = false; _preparingDocuments--;
            if (work.View is { } view && ReferenceEquals(view.Work, work))
            {
                view.Preparing = false;
                if (!IsSelected(work)) DeactivateView(view);
            }
            work.Completion.TrySetResult();
            if (work.Closed) work.Cancellation.Dispose();
            if (IsSelected(work)) { PresentDocument(work); Busy(false); }
            DispatchDocuments();
        }
    }

    private async Task LoadMoreAsync()
    {
        if (_activeKey is not { } key || !_documentWork.TryGetValue(key, out var work)
            || work.Preview is not { HasMore: true, NextOffset: int offset } previous) return;
        try
        {
            _more.IsEnabled = false;
            var next = work.Attachment is { } attachment && work.MessageId is { } messageId
                ? await _attachments.ReadPreviewAsync(work.ConversationId, messageId, attachment, offset, work.Cancellation.Token)
                : await _api.ReadAsync(work.ConversationId, work.Path, offset, work.UserSelected, work.Cancellation.Token, work.MountId);
            if (!IsLive(work)) return;
            if (next.Version != previous.Version) { if (IsSelected(work)) BeginRead(); return; }
            if (IsSelected(work)) SaveViewState();
            work.Preview = next with { Text = (previous.Text ?? "") + (next.Text ?? "") };
            CacheDocument(work);
            await QueueDocument(work);
        }
        catch (OperationCanceledException) { }
        catch (Exception error) { if (IsSelected(work)) Fail(error); }
        finally { if (IsSelected(work)) _more.IsEnabled = true; }
    }

    private async Task ToggleSourceAsync()
    {
        if (_activeKey is not { } key || !_documentWork.TryGetValue(key, out var work)) return;
        SaveViewState(); work.ShowSource = !work.ShowSource; _showSource = work.ShowSource;
        CacheDocument(work);
        await QueueDocument(work);
    }

    private async Task RenderDocumentAsync(DocumentWork work, CancellationToken cancellationToken)
    {
        await _loaded.Task.WaitAsync(cancellationToken);
        if (!IsLive(work) || work.Preview is null) return;
        var view = GetView(work.Key, website: work.Preview.Kind == "html");
        work.View = view; view.Work = work; view.Preparing = true; view.ContentAddress = work.ContentAddress;
        view.Browser.Visibility = Visibility.Visible;
        if (!IsSelected(work)) { view.Browser.Opacity = 0; view.Browser.IsHitTestVisible = false; }
        view.Browser.CoreWebView2?.Resume();
        view.Initialization ??= view.IsWebsite ? InitializeWebsiteAsync(view) : InitializeDocumentAsync(view);
        await view.Initialization.WaitAsync(cancellationToken);
        if (!IsLive(work)) return;
        view.Browser.CoreWebView2!.Resume();
        if (IsSelected(work)) PresentDocument(work);
        while (IsLive(work) && work.Preview is { } preview)
        {
            bool source = work.ShowSource;
            string signature = $"{preview.Kind}:{preview.Version}:{source}:{preview.Text?.Length ?? 0}";
            if (view.RenderedSignature == signature) return;
            if ((preview.Kind == "pdf" && !source || preview.Kind == "html") && work.ContentAddress is { } address)
            {
                view.BackgroundPreparing = false;
                Uri target = _navigationUris.TryGetValue(work.Key, out var saved) && IsViewContentAddress(view, saved.AbsoluteUri) ? saved : address;
                if (view.Browser.CoreWebView2.Source != target.AbsoluteUri)
                {
                    view.Navigated = NewReady(); view.Browser.CoreWebView2.Navigate(target.AbsoluteUri);
                    await view.Navigated.Task.WaitAsync(TimeSpan.FromSeconds(45), cancellationToken);
                }
                view.RenderedSignature = signature;
            }
            else
            {
                if (view.Browser.CoreWebView2.Source != PreviewPage)
                {
                    view.Ready = NewReady(); view.Browser.CoreWebView2.Navigate(PreviewPage);
                }
                await view.Ready.Task.WaitAsync(TimeSpan.FromSeconds(25), cancellationToken);
                string text = preview.Text ?? "";
                string? language = preview.Language ?? CodeLanguage(preview.Path);
                bool code = preview.Kind == "code" || preview.Kind == "text" && language is not null;
                string html = preview.Kind is "markdown" or "mermaid" && !source
                    ? await Task.Run(() => TranscriptMarkdown.Render(preview.Kind == "mermaid" ? "```mermaid\n" + text + "\n```" : text), cancellationToken)
                    : code ? await Task.Run(() => HighlightCode(text, language), cancellationToken) : "";
                if (!IsLive(work)) return;
                if (work.ShowSource != source || !ReferenceEquals(work.Preview, preview)) continue;
                view.Rendered = NewReady(); view.BackgroundPreparing = false;
                view.Browser.CoreWebView2.PostWebMessageAsJson(JsonSerializer.Serialize(new { type = "show", key = work.Key,
                    renderId = ++view.RenderId,
                    kind = source ? "text" : code ? "code" : preview.Kind, text, html,
                    contentUrl = IsDirectOfficeDocument(preview.Kind) ? DocumentContentAddress(preview.Kind, preview.Version) : work.ContentAddress?.AbsoluteUri,
                    state = _viewStates.GetValueOrDefault(work.Key), title = work.Title ?? preview.Title }));
                try { await view.Rendered.Task.WaitAsync(TimeSpan.FromSeconds(45), cancellationToken); }
                catch (Exception error) when (IsDirectOfficeDocument(preview.Kind)
                    && error is DirectDocumentPreviewException or TimeoutException)
                {
                    // A failed direct render changes only this file's representation, never the user's selected tab.
                    // 直接渲染失败只替换本文件的表示形式，不切换用户当前标签，也不重复打开产物。
                    if (!IsLive(work)) return;
                    await RequestPdfFallbackAsync(work);
                    if (!IsLive(work)) return;
                    continue;
                }
                view.RenderedSignature = signature;
            }
            if (work.ShowSource == source && ReferenceEquals(work.Preview, preview)) return;
        }
    }

    private Task RequestPdfFallbackAsync(DocumentWork work) => work.PdfFallback ??= ReadPdfFallbackAsync(work);

    private async Task ReadPdfFallbackAsync(DocumentWork work)
    {
        var token = work.Cancellation.Token;
        var preview = work.Attachment is { } attachment && work.MessageId is { } messageId
            ? await _attachments.ReadPreviewAsync(work.ConversationId, messageId, attachment,
                cancellationToken: token, representation: "pdf")
            : await _api.ReadAsync(work.ConversationId, work.Path, userSelected: work.UserSelected,
                cancellationToken: token, mountId: work.MountId, representation: "pdf");
        if (!IsLive(work)) return;
        if (IsDirectOfficeDocument(preview.Kind)) throw new DirectDocumentPreviewException();
        work.Preview = preview;
        work.ContentAddress = ContentAddress(work, preview.ContentUrl);
        if (work.View is { } view) view.ContentAddress = work.ContentAddress;
        CacheDocument(work);
    }

    private async Task RequeuePdfFallbackAsync(DocumentWork work)
    {
        try
        {
            // A later page can fail after the initial shown receipt; coalesce the same fallback and retain tab ownership.
            // 后续页可能在首屏 shown 回执后失败；合并同一文件的转换请求，保留原标签与当前选择。
            await RequestPdfFallbackAsync(work);
            if (work.Preparing) await work.Completion.Task;
            if (!IsLive(work)) return;
            await QueueDocument(work);
        }
        catch (OperationCanceledException) when (work.Cancellation.IsCancellationRequested) { }
        catch (Exception error)
        {
            if (IsLive(work)) { work.Error = error.Message; if (IsSelected(work)) Fail(error); }
        }
    }

    private void SetPreviewActions(WorkspacePreviewTarget preview)
    {
        _source.Visibility = preview.Kind is "markdown" or "mermaid" ? Visibility.Visible : Visibility.Collapsed;
        _more.Visibility = preview.HasMore ? Visibility.Visible : Visibility.Collapsed;
        _source.Content = _showSource ? UiText.Get("显示预览") : UiText.Get("查看源码");
    }

    private static string? CodeLanguage(string path) => Path.GetExtension(path).ToLowerInvariant() switch
    {
        ".cs" => "csharp", ".rs" => "rust", ".js" or ".mjs" or ".cjs" => "javascript",
        ".ts" or ".tsx" => "typescript", ".jsx" => "javascript", ".py" => "python",
        ".json" or ".jsonc" => "json", ".xml" or ".xaml" or ".csproj" or ".svg" => "xml",
        ".css" => "css", ".html" or ".htm" => "html", ".ps1" => "powershell", ".sh" => "bash",
        ".c" or ".h" or ".cpp" or ".hpp" => "cpp", ".java" => "java", ".go" => "go",
        ".sql" => "sql", ".yaml" or ".yml" => "yaml", ".toml" => "toml", ".diff" or ".patch" => "diff",
        _ => null
    };

    private static string HighlightCode(string text, string? language)
    {
        var html = new StringBuilder("<pre class=\"source-code\"><code>");
        foreach (var token in CodeSyntaxHighlighter.Highlight(text, language))
        {
            if (token.Color is { } color) html.Append("<span style=\"color:#").Append((color & 0xFFFFFF).ToString("x6")).Append("\">");
            html.Append(WebUtility.HtmlEncode(token.Text));
            if (token.Color.HasValue) html.Append("</span>");
        }
        return html.Append("</code></pre>").ToString();
    }

    private async Task InitializeDocumentAsync(BrowserView view)
    {
        var browser = view.Browser;
        _viewport.Children.Insert(0, browser);
        _previewEnvironment ??= CreateEnvironmentAsync(Path.Combine(ApplicationPaths.WebViewDirectory, "WorkspacePreview"));
        var environment = await _previewEnvironment;
        if (_disposed || view.Closed) return;
        await browser.EnsureCoreWebView2Async(environment);
        if (_disposed || view.Closed) return;
        var core = browser.CoreWebView2;
        core.Settings.AreDevToolsEnabled = false; core.Settings.AreHostObjectsAllowed = false;
        core.Settings.IsPasswordAutosaveEnabled = false; core.Settings.IsGeneralAutofillEnabled = false;
        core.Settings.IsStatusBarEnabled = false; core.Settings.IsWebMessageEnabled = true;
        core.SetVirtualHostNameToFolderMapping(PreviewHost, Path.Combine(AppContext.BaseDirectory, "Resources"),
            CoreWebView2HostResourceAccessKind.DenyCors);
        core.NavigationStarting += (sender, args) =>
        {
            if (args.Uri == PreviewPage || IsViewContentAddress(view, args.Uri)) return;
            args.Cancel = true;
            if (TryWebsite(args.Uri, out _)) OpenLink(args.Uri);
        };
        core.NewWindowRequested += (sender, args) => { args.Handled = true; if (TryWebsite(args.Uri, out _)) OpenLink(args.Uri); };
        core.PermissionRequested += (_, args) => args.State = CoreWebView2PermissionState.Deny;
        core.DownloadStarting += (_, args) => args.Cancel = true;
        core.AddWebResourceRequestedFilter("*", CoreWebView2WebResourceContext.All);
        core.WebResourceRequested += (sender, args) =>
        {
            if (Uri.TryCreate(args.Request.Uri, UriKind.Absolute, out var documentUri)
                && documentUri.Scheme == "https" && documentUri.Host == DocumentContentHost
                && documentUri.AbsolutePath is "/Workspace/document.docx" or "/Workspace/document.pptx" or "/Workspace/document.xlsx")
            {
                var deferral = args.GetDeferral();
                _ = SupplyDocumentContentAsync(view, args, environment, deferral.Complete);
                return;
            }
            if (Uri.TryCreate(args.Request.Uri, UriKind.Absolute, out var uri) &&
                (uri.Scheme == "https" && uri.Host == PreviewHost || IsViewContentAddress(view, args.Request.Uri)
                    || uri.Scheme is "chrome-extension" or "edge-extension" or "data" or "blob")) return;
            args.Response = environment.CreateWebResourceResponse(null, 403, "Forbidden", "");
        };
        core.WebMessageReceived += (sender, args) => PreviewMessageReceived(view, sender, args);
        core.NavigationCompleted += (_, args) =>
        {
            if (args.IsSuccess && IsViewContentAddress(view, core.Source)) view.Navigated.TrySetResult();
            if (!_disposed && _enabled && _activeKey == view.Key && !_isWebsite && IsViewContentAddress(view, core.Source)) Reveal(browser);
        };
        core.Navigate(PreviewPage);
        await view.Ready.Task.WaitAsync(TimeSpan.FromSeconds(20));
        ApplyAppearance();
    }

    private static bool IsDirectOfficeDocument(string kind) => kind is "docx" or "pptx" or "xlsx";

    private static string DocumentContentAddress(string kind, string version) =>
        "https://" + DocumentContentHost + "/Workspace/document." + kind + "?version=" + Uri.EscapeDataString(version);

    private async Task SupplyDocumentContentAsync(BrowserView view, CoreWebView2WebResourceRequestedEventArgs args,
        CoreWebView2Environment environment, Action completeDeferral)
    {
        try
        {
            var work = view.Work;
            if (args.Request.Method != "GET" || work?.Preview is not { } preview || !IsDirectOfficeDocument(preview.Kind)
                || !IsLive(work) || view.Browser.CoreWebView2.Source != PreviewPage
                || args.Request.Uri != DocumentContentAddress(preview.Kind, preview.Version) || work.ContentAddress is not { } address)
            {
                args.Response = environment.CreateWebResourceResponse(null, 403, "Forbidden", "");
                return;
            }
            // Mapped static hosts bypass this event; a separate host serves only this view's verified original.
            // 静态目录映射不会触发此事件；独立代理域只返回此视图已核验的原件，不开放其他文件或地址。
            var bytes = await _api.ReadDocumentContentAsync(address, work.Cancellation.Token);
            if (!IsLive(work) || !ReferenceEquals(work.Preview, preview))
            {
                args.Response = environment.CreateWebResourceResponse(null, 409, "Conflict", "");
                return;
            }
            string mimeType = preview.Kind switch
            {
                "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
                "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                _ => "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            };
            args.Response = environment.CreateWebResourceResponse(new MemoryStream(bytes, writable: false).AsRandomAccessStream(), 200, "OK",
                $"Content-Type: {mimeType}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nAccess-Control-Allow-Origin: https://kynxa-workspace.local\r\n");
        }
        catch (Exception error)
        {
            Debug.WriteLine($"Workspace Office content: {error.GetType().Name}");
            try { args.Response = environment.CreateWebResourceResponse(null, 502, "Document unavailable", ""); }
            catch (Exception closed) { Debug.WriteLine($"Workspace Office response: {closed.GetType().Name}"); }
        }
        finally
        {
            try { completeDeferral(); }
            catch (Exception closed) { Debug.WriteLine($"Workspace Office deferral: {closed.GetType().Name}"); }
        }
    }

    private Task OpenWebsiteAsync(Uri address, long generation) => OpenChromeAsync(address, generation);

    private async Task InitializeWebsiteAsync(BrowserView view)
    {
        var browser = view.Browser;
        _viewport.Children.Insert(0, browser);
        // Local HTML remains an isolated document renderer, without browser automation or a native message bridge.
        // 本地 HTML 保持独立文档视图，不开放浏览器自动化或原生消息桥。
        _websiteEnvironment ??= CreateEnvironmentAsync(Path.Combine(ApplicationPaths.WebViewDirectory, "WorkspaceHtml"));
        await browser.EnsureCoreWebView2Async(await _websiteEnvironment);
        if (_disposed || view.Closed) return;
        var core = browser.CoreWebView2;
        core.Settings.AreHostObjectsAllowed = false; core.Settings.IsWebMessageEnabled = false;
        core.Settings.AreDevToolsEnabled = false; core.Settings.IsStatusBarEnabled = false;
        core.Settings.IsPasswordAutosaveEnabled = false; core.Settings.IsGeneralAutofillEnabled = false;
        core.NavigationStarting += (sender, args) => { if (args.Uri != "about:blank" && !TryWebsite(args.Uri, out _)) args.Cancel = true; };
        core.NewWindowRequested += (sender, args) => { args.Handled = true; if (TryWebsite(args.Uri, out _)) OpenLink(args.Uri); };
        core.PermissionRequested += (_, args) => args.State = CoreWebView2PermissionState.Deny;
        core.DownloadStarting += (_, args) => args.Cancel = true;
        core.SourceChanged += (_, _) => { if (ReferenceEquals(_website, browser)) UpdateWebsiteAddress(); };
        core.HistoryChanged += (_, _) => { if (ReferenceEquals(_website, browser)) UpdateWebsiteAddress(); };
        core.NavigationCompleted += (_, args) =>
        {
            if (args.IsSuccess && IsViewContentAddress(view, core.Source)) view.Navigated.TrySetResult();
            if (!ReferenceEquals(_website, browser)) return;
            UpdateWebsiteAddress(); RestoreWebsiteScroll();
            if (!_disposed && _enabled && (_isWebsite || _current?.Kind == "html")) Reveal(browser);
        };
    }

    private async Task OpenChromeAsync(Uri address, long generation)
    {
        string key = _activeKey!;
        try
        {
            if (_clearingWebsiteData) return;
            _openWebsite.Visibility = Visibility.Collapsed;
            HideOldContent(); Busy(true, "正在打开网页…");
            await _loaded.Task;
            if (!IsCurrent(generation)) return;
            if (!_chromeViews.TryGetValue(key, out var tab))
            {
                var browser = new ConversationChromeView();
                _viewport.Children.Insert(0, browser);
                browser.NavigationChanged += (_, state) =>
                {
                    if (TryWebsite(state.Url, out var current)) _navigationUris[key] = current;
                    if (!_disposed && _isWebsite && ReferenceEquals(_chrome, browser))
                    {
                        if (_address.FocusState == FocusState.Unfocused && TryWebsite(state.Url, out var displayed))
                            _address.Text = displayed.AbsoluteUri;
                        _back.IsEnabled = state.CanGoBack; _forward.IsEnabled = state.CanGoForward;
                        ShowVerificationState(state);
                    }
                };
                tab = new(browser, browser.OpenAsync(_conversationId, address));
                _chromeViews[key] = tab;
            }
            tab.Touched = ++_chromeSequence;
            _chrome = tab.View;
            await tab.Initialization;
            if (!IsCurrent(generation)) return;
            foreach (var cached in _chromeViews.Values)
            {
                bool active = ReferenceEquals(cached.View, _chrome);
                cached.View.Visibility = active ? Visibility.Visible : Visibility.Collapsed;
                cached.View.SetActive(active);
            }
            foreach (var cached in _views.Values)
            {
                DeactivateView(cached);
            }
            _address.Text = _chrome.Address ?? address.AbsoluteUri;
            SetToolbarMode(true);
            await _chrome.NavigateAsync("state");
            while (_chromeViews.Count > Math.Clamp(MaximumRetainedViews / 2, 2, 4))
            {
                var oldest = _chromeViews.Where(item => item.Key != key && item.Value.Initialization.IsCompleted)
                    .MinBy(item => item.Value.Touched);
                if (oldest.Value is null) break;
                _chromeViews.Remove(oldest.Key); _viewport.Children.Remove(oldest.Value.View); CloseChromeView(oldest.Value.View);
            }
        }
        catch (OperationCanceledException) { }
        catch (Exception error)
        {
            if (_chromeViews.Remove(key, out var failed)) { _viewport.Children.Remove(failed.View); CloseChromeView(failed.View); }
            if (IsCurrent(generation)) { _chrome = null; Fail(error); }
        }
        finally { if (IsCurrent(generation)) Busy(false); }
    }

    private async Task NavigateChromeAsync(string action, Uri? address = null)
    {
        try { if (_isWebsite && _chrome is { } browser) await browser.NavigateAsync(action, address); }
        catch (OperationCanceledException) { }
        catch (Exception error) { if (!_disposed) Fail(error); }
    }

    private void PreviewMessageReceived(BrowserView view, CoreWebView2 sender, CoreWebView2WebMessageReceivedEventArgs args)
    {
        if (_disposed || view.Closed || args.Source != PreviewPage) return;
        try
        {
            using var document = JsonDocument.Parse(args.WebMessageAsJson);
            var message = document.RootElement;
            string? type = message.GetProperty("type").GetString();
            if (type == "ready") { view.Ready.TrySetResult(); if (ReferenceEquals(_document, view.Browser)) ApplyAppearance(); return; }
            if (!message.TryGetProperty("key", out var key) || key.GetString() is not { Length: > 0 } messageKey) return;
            // A saved scroll reply can arrive after the user has already selected the next tab.
            // 滚动保存回执可能晚于标签切换，仍按原标签键保存，而不是丢弃或归到新标签。
            if (type == "state" && message.TryGetProperty("state", out var state)) { CacheState(messageKey, state.GetRawText()); return; }
            if (type == "shown")
            {
                if (messageKey != view.Key || message.TryGetProperty("renderId", out var renderId) && renderId.GetInt64() != view.RenderId) return;
                view.Rendered.TrySetResult();
                if (_enabled && messageKey == _activeKey && !_isWebsite) Reveal(view.Browser);
                return;
            }
            if (type is "preparing" or "settled")
            {
                if (messageKey != view.Key || view.Browser.CoreWebView2.Source != PreviewPage
                    || !message.TryGetProperty("renderId", out var backgroundRender) || backgroundRender.GetInt64() != view.RenderId) return;
                view.BackgroundPreparing = type == "preparing";
                if (view.BackgroundPreparing) view.Browser.CoreWebView2.Resume();
                if (!view.Active) DeactivateView(view);
                if (!view.BackgroundPreparing) { TrimRetainedViews(); DispatchDocuments(); }
                return;
            }
            if (type is "docx-failed" or "pptx-failed" or "xlsx-failed")
            {
                if (messageKey == view.Key && view.Work is { } failedWork && IsLive(failedWork)
                    && failedWork.Preview is { } failedPreview && IsDirectOfficeDocument(failedPreview.Kind)
                    && type == failedPreview.Kind + "-failed"
                    && message.TryGetProperty("renderId", out var failedRender) && failedRender.GetInt64() == view.RenderId)
                {
                    if (!view.Rendered.TrySetException(new DirectDocumentPreviewException())
                        && view.Rendered.Task.IsCompletedSuccessfully && !failedWork.PdfFallbackQueued)
                    {
                        failedWork.PdfFallbackQueued = true;
                        _ = RequeuePdfFallbackAsync(failedWork);
                    }
                }
                return;
            }
            if (messageKey != _activeKey) return;
            if (type == "link" && message.TryGetProperty("url", out var url) && TryWebsite(url.GetString(), out _)) OpenLink(url.GetString()!);
            else if (type == "copy" && message.TryGetProperty("text", out var text))
            {
                var data = new DataPackage(); data.SetText(text.GetString() ?? ""); Clipboard.SetContent(data);
            }
        }
        catch (Exception error) when (error is JsonException or InvalidOperationException or ArgumentException)
        { Debug.WriteLine($"Workspace preview message: {error.GetType().Name}"); }
    }

    private void SaveViewState()
    {
        if (_activeKey is not { } key) return;
        if (_isWebsite && _chrome is { } chrome)
        {
            if (TryWebsite(chrome.Address, out var source)) _navigationUris[key] = source;
            return;
        }
        if (!_views.TryGetValue(key, out var view) || view.Browser.CoreWebView2 is not { } core) return;
        if (view.IsWebsite)
        {
            CacheState(key, _viewStates.GetValueOrDefault(key, "{}"));
            _ = SaveWebsiteStateAsync(core, key);
        }
        else if (core.Source == PreviewPage) core.PostWebMessageAsJson(JsonSerializer.Serialize(new { type = "saveState" }));
        else if (IsViewContentAddress(view, core.Source) && Uri.TryCreate(core.Source, UriKind.Absolute, out var pdf))
        { CacheState(key, "{}"); _navigationUris[key] = pdf; }
    }

    private async Task SaveWebsiteStateAsync(CoreWebView2 core, string key)
    {
        try { CacheState(key, await core.ExecuteScriptAsync("JSON.stringify({x:scrollX,y:scrollY})")); }
        catch (Exception error) { Debug.WriteLine($"Workspace browser scroll: {error.GetType().Name}"); }
    }

    private void RestoreWebsiteScroll()
    {
        if (_website?.CoreWebView2 is not { } core || SavedState() is not { } state) return;
        _ = RestoreWebsiteScrollAsync(core, state);
    }

    private static async Task RestoreWebsiteScrollAsync(CoreWebView2 core, string state)
    {
        try
        {
            string decoded = JsonSerializer.Deserialize<string>(state) ?? state;
            using var document = JsonDocument.Parse(decoded);
            if (document.RootElement.TryGetProperty("x", out var x) && document.RootElement.TryGetProperty("y", out var y)
                && x.TryGetDouble(out double left) && y.TryGetDouble(out double top) && double.IsFinite(left) && double.IsFinite(top))
                await core.ExecuteScriptAsync($"scrollTo({JsonSerializer.Serialize(left)},{JsonSerializer.Serialize(top)})");
        }
        catch (Exception error) { Debug.WriteLine($"Workspace browser restore: {error.GetType().Name}"); }
    }

    // A cached tab keeps its DOM, page navigation and selection; inactive WebViews are suspended when possible.
    // 缓存标签保留 DOM、翻页和选区；闲置 WebView 尽量挂起，达到硬件档位后回收最久未用的视图。
    private BrowserView GetView(string key, bool website)
    {
        if (_views.TryGetValue(key, out var cached)) { cached.LastUsed = ++_viewClock; return cached; }
        var view = new BrowserView(key, website) { LastUsed = ++_viewClock };
        _views[key] = view;
        TrimRetainedViews(key);
        return view;
    }

    private void TrimRetainedViews(string? retainedKey = null)
    {
        while (_views.Count > MaximumRetainedViews)
        {
            var oldest = _views.Values.Where(item => item.Key != retainedKey && !item.Active && !item.Preparing
                && !item.BackgroundPreparing && item.Initialization?.IsCompleted != false)
                .OrderBy(item => item.LastUsed).FirstOrDefault();
            if (oldest is null) break;
            _views.Remove(oldest.Key); oldest.Closed = true;
            _viewport.Children.Remove(oldest.Browser); oldest.Browser.Close();
        }
    }

    private void ActivateView(WebView2 active)
    {
        foreach (var tab in _chromeViews.Values) tab.View.SetActive(false);
        foreach (var view in _views.Values)
        {
            bool selected = ReferenceEquals(view.Browser, active);
            view.Active = selected;
            if (selected) view.Browser.Visibility = Visibility.Visible;
            else DeactivateView(view);
            if (view.Browser.CoreWebView2 is not { } core) continue;
            core.MemoryUsageTargetLevel = selected ? CoreWebView2MemoryUsageTargetLevel.Normal : CoreWebView2MemoryUsageTargetLevel.Low;
            if (selected) core.Resume();
        }
    }

    private static void DeactivateView(BrowserView view)
    {
        view.Active = false; view.Browser.Opacity = 0; view.Browser.IsHitTestVisible = false;
        // First-paint admission and background settling are separate: keep unfinished hidden work alive without taking input.
        // 首屏准入与后台完成分开；未完成的隐藏视图继续准备，但不能抢占用户输入。
        view.Browser.Visibility = view.Preparing || view.BackgroundPreparing ? Visibility.Visible : Visibility.Collapsed;
        if (!view.Preparing && !view.BackgroundPreparing) _ = SuspendAsync(view);
    }

    private void CacheState(string key, string state)
    {
        if (_disposed || state.Length > 2048) return;
        if (!_viewStates.ContainsKey(key)) _viewOrder.Enqueue(key);
        _viewStates[key] = state;
        while (_viewOrder.Count > 128)
        {
            string removed = _viewOrder.Dequeue(); _viewStates.Remove(removed); _navigationUris.Remove(removed);
        }
    }

    private string? SavedState() => _activeKey is { } key && _viewStates.TryGetValue(key, out var state) ? state : null;
    private bool IsCurrent(long generation) => !_disposed && _generation == generation && _enabled;
    private static bool IsViewContentAddress(BrowserView view, string value) => view.ContentAddress is { } content
        && Uri.TryCreate(value, UriKind.Absolute, out var address) && address.GetLeftPart(UriPartial.Path) == content.GetLeftPart(UriPartial.Path)
        && address.Query == content.Query;

    private void OpenLink(string url)
    {
        if (WebsiteRequested is { } handler) handler(this, url);
        else ShowWebsite(_conversationId, url);
    }

    private void RefreshCurrent()
    {
        if (_isWebsite && !_websiteActivationRequested) ActivateWebsite();
        else if (_isWebsite && _chrome is null && _websiteAddress is { } address) _ = OpenChromeAsync(address, ++_generation);
        else if (_isWebsite) _ = NavigateChromeAsync("reload");
        else { SaveViewState(); BeginRead(); }
    }

    private void UpdateWebsiteAddress()
    {
        if (_website?.CoreWebView2 is not { } core) return;
        if (_websiteKey is { } key && TryWebsite(core.Source, out var actualSource))
        {
            CacheState(key, _viewStates.GetValueOrDefault(key, "{}")); _navigationUris[key] = actualSource;
        }
        if (!_isWebsite || _activeKey != _websiteKey) return;
        _address.Text = core.Source;
        if (TryWebsite(core.Source, out var address)) _websiteAddress = address;
        _back.IsEnabled = core.CanGoBack; _forward.IsEnabled = core.CanGoForward;
    }

    private void AddressKeyDown(object sender, KeyRoutedEventArgs args)
    {
        if (args.Key != VirtualKey.Enter) return;
        if (WorkspaceNavigationAddress.TryResolve(_address.Text, out var address))
            _ = NavigateChromeAsync("navigate", address);
        args.Handled = true;
    }

    private static bool TryWebsite(string? value, out Uri address)
    {
        bool valid = Uri.TryCreate(value, UriKind.Absolute, out var result) && result.Scheme is "http" or "https" && result.UserInfo.Length == 0;
        address = result!; return valid;
    }

    private void SetToolbarMode(bool website)
    {
        _back.Visibility = _forward.Visibility = _address.Visibility = website ? Visibility.Visible : Visibility.Collapsed;
        _browserOptions.Visibility = website ? Visibility.Visible : Visibility.Collapsed;
        _source.Visibility = _more.Visibility = Visibility.Collapsed;
        if (!website)
        {
            _openWebsite.Visibility = Visibility.Collapsed;
            _showingVerification = false; _alternativeSearchAddress = null;
            _alternativeSearch.Visibility = Visibility.Collapsed;
        }
    }

    private void ShowWebsiteDataMenu()
    {
        var menu = new MenuFlyout();
        var clear = new MenuFlyoutItem { Text = UiText.Get("清除所有网站登录数据"), IsEnabled = !_clearingWebsiteData };
        clear.Click += async (_, _) => await ClearWebsiteDataAsync();
        menu.Items.Add(clear);
        menu.ShowAt(_browserOptions);
    }

    private async Task ClearWebsiteDataAsync()
    {
        if (_disposed || _clearingWebsiteData || _conversationId == Guid.Empty) return;
        _clearingWebsiteData = true; _browserOptions.IsEnabled = false;
        try
        {
            // Login data belongs to the application, while closing a tab never removes it.
            // 登录数据由整个应用共享；仅此明确操作清除，关闭标签和删除聊天不触碰登录目录。
            await CloseAllAsync();
            using var client = new ChromeWorkspaceApiClient();
            await client.ClearWebsiteDataAsync(_conversationId);
            _websiteActivationRequested = false;
            if (!_disposed && _isWebsite)
            {
                Busy(false); _openWebsite.Visibility = Visibility.Visible;
                SetNotice(UiText.Get("所有工作和聊天共用的网站登录数据已清除。"));
            }
        }
        catch (Exception error) { if (!_disposed) Fail(error); }
        finally { _clearingWebsiteData = false; if (!_disposed) _browserOptions.IsEnabled = true; }
    }

    private void ShowVerificationState(ChromeWorkspaceState state)
    {
        bool previous = _showingVerification;
        _showingVerification = state.NeedsHumanVerification;
        _alternativeSearchAddress = state.NeedsHumanVerification && TryWebsite(state.AlternativeSearchUrl, out var alternative) ? alternative : null;
        _alternativeSearch.Visibility = _alternativeSearchAddress is not null ? Visibility.Visible : Visibility.Collapsed;
        if (_showingVerification) SetNotice(UiText.Get(_alternativeSearchAddress is null
            ? "此网页需要人工验证，请在网页中完成。" : "搜索服务需要人工验证，可在网页完成验证或切换搜索源。"));
        else if (previous) SetNotice(null);
    }

    private void AddToolbar(FrameworkElement child, int column) { SetColumn(child, column); _toolbar.Children.Add(child); }

    private static Task<CoreWebView2Environment> CreateEnvironmentAsync(string folder, CoreWebView2EnvironmentOptions? options = null)
    {
        // WebView's global folder override would otherwise merge the trusted and CDP-enabled browser processes.
        // WebView 全局目录变量会覆盖显式目录，将可信视图与开启 CDP 的浏览器混用；仅在同步创建入口暂时屏蔽。
        string? overriddenFolder = Environment.GetEnvironmentVariable("WEBVIEW2_USER_DATA_FOLDER");
        try
        {
            Environment.SetEnvironmentVariable("WEBVIEW2_USER_DATA_FOLDER", null);
            return CoreWebView2Environment.CreateWithOptionsAsync(null, folder, options).AsTask();
        }
        finally { Environment.SetEnvironmentVariable("WEBVIEW2_USER_DATA_FOLDER", overriddenFolder); }
    }

    private async Task WarmPreviewEnvironmentAsync()
    {
        if (_disposed || _previewEnvironment is not null) return;
        Task<CoreWebView2Environment>? preparation = null;
        try
        {
            // Low-priority environment-only preparation creates no document controller and is never awaited by chat or startup.
            // 低优先级仅准备共享环境，不创建文档控制器，也不进入聊天或启动的等待链路。
            preparation = _previewEnvironment ??= CreateEnvironmentAsync(Path.Combine(ApplicationPaths.WebViewDirectory, "WorkspacePreview"));
            await preparation;
        }
        catch (Exception error)
        {
            if (ReferenceEquals(_previewEnvironment, preparation)) _previewEnvironment = null;
            Debug.WriteLine($"Workspace environment warmup: {error.GetType().Name}");
        }
    }
    private static Button QuietIcon(string glyph, string label)
    {
        var button = QuietButton("", label);
        button.Content = new FontIcon { Glyph = glyph, FontSize = 13 };
        return button;
    }

    private static Button QuietButton(string content, string label)
    {
        var button = new Button { Content = content, MinWidth = 28, MinHeight = 28, Padding = new Thickness(7, 3, 7, 3),
            CornerRadius = new CornerRadius(7), BorderThickness = new Thickness(0), FontSize = 12 };
        if (Application.Current.Resources.TryGetValue("KynxaQuietButtonStyle", out var value) && value is Style style) button.Style = style;
        LabelButton(button, label);
        return button;
    }

    private static void LabelButton(Button button, string key)
    {
        AutomationProperties.SetName(button, UiText.Get(key)); ToolTipService.SetToolTip(button, UiText.Get(key));
    }

    private void AppearanceChanged(object? sender, EventArgs args) => DispatcherQueue.TryEnqueue(ApplyAppearance);
    private void LanguageChanged(object? sender, EventArgs args) => DispatcherQueue.TryEnqueue(() =>
    {
        _more.Content = UiText.Get("继续加载"); _source.Content = UiText.Get(_showSource ? "显示预览" : "查看源码");
        LabelButton(_back, "后退"); LabelButton(_forward, "前进"); LabelButton(_reload, "刷新预览");
        LabelButton(_source, _showSource ? "显示预览" : "查看源码"); LabelButton(_more, "继续加载");
        LabelButton(_browserOptions, "网站数据"); _openWebsite.Content = UiText.Get("打开网页"); LabelButton(_openWebsite, "打开网页");
        _alternativeSearch.Content = UiText.Get("切换搜索源"); LabelButton(_alternativeSearch, "切换搜索源");
        if (_showingVerification) SetNotice(UiText.Get(_alternativeSearchAddress is null
            ? "此网页需要人工验证，请在网页中完成。" : "搜索服务需要人工验证，可在网页完成验证或切换搜索源。"));
        AutomationProperties.SetName(_address, UiText.Get("网页地址"));
    });

    private void ApplyAppearance()
    {
        if (_disposed) return;
        var palette = AppearanceService.Current;
        var border = new SolidColorBrush(AppearanceService.ParseColor(palette.Border));
        _address.Resources["TextControlBorderBrushFocused"] = border;
        _address.Resources["TextControlBorderBrushPointerOver"] = border;
        _address.FocusVisualPrimaryBrush = border; _address.FocusVisualSecondaryBrush = border;
        _notice.Foreground = _loading.Foreground = new SolidColorBrush(AppearanceService.ParseColor(palette.Secondary));
        var appearance = new { type = "appearance", palette = new { main = palette.Main, soft = palette.Soft, accent = palette.Secondary,
            text = palette.Text, secondary = palette.Secondary, border = palette.Border, sidebar = palette.Sidebar } };
        foreach (var view in _views.Values)
        {
            view.Browser.DefaultBackgroundColor = AppearanceService.ParseColor(palette.Main);
            if (!view.IsWebsite && view.Browser.CoreWebView2?.Source == PreviewPage)
                view.Browser.CoreWebView2.PostWebMessageAsJson(JsonSerializer.Serialize(appearance));
        }
    }

    private void Post(object message)
    {
        if (!_disposed && _document?.CoreWebView2?.Source == PreviewPage)
            _document.CoreWebView2.PostWebMessageAsJson(JsonSerializer.Serialize(message));
    }

    private void SetNotice(string? text) { _notice.Text = text ?? ""; _notice.Visibility = string.IsNullOrWhiteSpace(text) ? Visibility.Collapsed : Visibility.Visible; }
    private void HideOldContent()
    {
        foreach (var tab in _chromeViews.Values) tab.View.SetActive(false);
        if (_document is not null) { _document.Opacity = 0; _document.IsHitTestVisible = false; }
        if (_website is not null) { _website.Opacity = 0; _website.IsHitTestVisible = false; }
    }
    private static void Reveal(WebView2 browser) { browser.Opacity = 1; browser.IsHitTestVisible = true; }
    private void Busy(bool busy, string? key = null) { if (key is not null) _loading.Text = UiText.Get(key); _loading.Visibility = busy ? Visibility.Visible : Visibility.Collapsed; }
    private void Fail(Exception error) { SetNotice(error.Message); Busy(false); }
    private static TaskCompletionSource NewReady() => new(TaskCreationOptions.RunContinuationsAsynchronously);
    private static async Task SuspendAsync(BrowserView view)
    {
        try
        {
            if (view.Closed || view.Active || view.Preparing || view.BackgroundPreparing || view.Browser.CoreWebView2 is not { } core) return;
            await core.TrySuspendAsync();
            // A fast switch may select this view before its earlier suspend operation has completed.
            // 快速切回可能早于旧挂起完成；以最新选择恢复，不能让晚到的挂起冻结当前页面。
            if (!view.Closed && (view.Active || view.Preparing || view.BackgroundPreparing)) core.Resume();
        }
        catch (Exception error) { Debug.WriteLine($"Workspace suspend: {error.GetType().Name}"); }
    }

    private void CloseDocumentWork(DocumentWork work)
    {
        if (work.Closed) return;
        work.Closed = true; work.Cancellation.Cancel();
        _pendingDocuments.Remove(work); work.Queued = false;
        if (!work.Preparing) { work.Completion.TrySetResult(); work.Cancellation.Dispose(); }
    }

    private void CloseView(string key)
    {
        if (!_views.Remove(key, out var view)) return;
        view.Closed = true; view.Ready.TrySetCanceled(); view.Rendered.TrySetCanceled(); view.Navigated.TrySetCanceled();
        _viewport.Children.Remove(view.Browser); view.Browser.Close();
    }

    public void CloseTab(Guid conversationId, ConversationWorkTab tab)
    {
        string? key = tab.Kind switch
        {
            "changes" => $"{conversationId:D}:changes",
            "browser" => $"{conversationId:D}:web:{tab.Target}",
            _ when tab.Target?.StartsWith("attachment:", StringComparison.Ordinal) == true => $"{conversationId:D}:{tab.Target}",
            _ when tab.Target is not null => $"{conversationId:D}:file:{tab.Target}",
            _ => null
        };
        if (key is null) return;
        if (_documentWork.Remove(key, out var work)) CloseDocumentWork(work);
        _documents.Remove(key); _viewStates.Remove(key); _navigationUris.Remove(key); CloseView(key);
        RemoveStateOrder(item => item == key);
        if (_chromeViews.Remove(key, out var chrome)) { _viewport.Children.Remove(chrome.View); CloseChromeView(chrome.View); }
    }

    public void CloseConversation(Guid conversationId)
    {
        string prefix = conversationId.ToString("D") + ":";
        foreach (var pair in _documentWork.Where(pair => pair.Key.StartsWith(prefix, StringComparison.Ordinal)).ToArray())
        { _documentWork.Remove(pair.Key); CloseDocumentWork(pair.Value); }
        foreach (string key in _views.Keys.Where(key => key.StartsWith(prefix, StringComparison.Ordinal)).ToArray()) CloseView(key);
        foreach (string key in _documents.Keys.Where(key => key.StartsWith(prefix, StringComparison.Ordinal)).ToArray()) _documents.Remove(key);
        foreach (string key in _viewStates.Keys.Where(key => key.StartsWith(prefix, StringComparison.Ordinal)).ToArray()) _viewStates.Remove(key);
        foreach (string key in _navigationUris.Keys.Where(key => key.StartsWith(prefix, StringComparison.Ordinal)).ToArray()) _navigationUris.Remove(key);
        RemoveStateOrder(key => key.StartsWith(prefix, StringComparison.Ordinal));
        foreach (var pair in _chromeViews.Where(pair => pair.Key.StartsWith(prefix, StringComparison.Ordinal)).ToArray())
        { _chromeViews.Remove(pair.Key); _viewport.Children.Remove(pair.Value.View); CloseChromeView(pair.Value.View); }
    }

    private void CloseChromeView(ConversationChromeView view)
    {
        _closingChrome.RemoveAll(task => task.IsCompleted);
        _closingChrome.Add(view.CloseAsync());
    }

    /// <summary>Wait for owned browser cleanup, including an open receipt still arriving from Rust.</summary>
    /// <remarks>等待本视图持有的浏览器完成关闭；尚未收到启动回执的会话也纳入退出等待。</remarks>
    public Task CloseAllAsync()
    {
        ++_generation;
        foreach (var tab in _chromeViews.Values)
        {
            _viewport.Children.Remove(tab.View);
            CloseChromeView(tab.View);
        }
        _chromeViews.Clear(); _chrome = null;
        return Task.WhenAll(_closingChrome.ToArray());
    }

    private void RemoveStateOrder(Func<string, bool> remove)
    {
        var retained = _viewOrder.Where(key => !remove(key)).ToArray();
        _viewOrder.Clear();
        foreach (string key in retained) _viewOrder.Enqueue(key);
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true; ++_generation;
        foreach (var work in _documentWork.Values) CloseDocumentWork(work);
        _documentWork.Clear(); _pendingDocuments.Clear();
        AppearanceService.Changed -= AppearanceChanged; UiText.LanguageChanged -= LanguageChanged;
        foreach (string key in _views.Keys.ToArray()) CloseView(key);
        _views.Clear(); _documents.Clear(); _attachments.Dispose();
        foreach (var tab in _chromeViews.Values) CloseChromeView(tab.View);
        _chromeViews.Clear(); _api.Dispose();
        _viewStates.Clear(); _navigationUris.Clear(); _viewOrder.Clear();
    }

}
