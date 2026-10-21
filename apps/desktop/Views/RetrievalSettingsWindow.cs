using System.Net;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Storage.Pickers;

namespace KYNXA_Desktop.Views;

/// <summary>
/// Edits retrieval settings through versioned gateway requests; never writes source files or chat state.
/// 通过带版本的网关请求编辑检索设置；不写入原始资料或聊天状态。
/// </summary>
public sealed partial class RetrievalSettingsWindow : Window
{
    private readonly IRetrievalApi _api;
    private readonly bool _ownsApi;
    private readonly Guid? _projectId;
    private readonly string? _projectName;
    private readonly string? _mountedPath;
    private readonly CancellationTokenSource _lifetime = new();
    private CancellationTokenSource? _observation;
    private RetrievalSettingsDocument? _global;
    private ProjectRetrievalSettingsDocument? _project;
    private RetrievalIndexJob? _job;
    private RetrievalStatus? _status;
    private string? _noticeMessageKey;
    private bool _closed, _busy, _rendering, _loaded, _polling, _statusPolling, _statusObservationFailed, _allowClose;
    private Window? _settingsOwner;

    public RetrievalSettingsWindow(Guid? projectId = null, string? projectName = null, string? mountedPath = null,
        IRetrievalApi? api = null, Window? settingsOwner = null)
    {
        if (projectId == Guid.Empty) throw new ArgumentException("A saved project ID is required.", nameof(projectId));
        _projectId = projectId;
        _projectName = projectName;
        _mountedPath = mountedPath;
        _api = api ?? new RetrievalApiClient();
        _ownsApi = api is null;
        _settingsOwner = settingsOwner;
        BuildLayout();
        if (settingsOwner is null) AppearanceService.TrackWindow(this);
        Title = UiText.Get("KYNXA · 检索与网页搜索");
        if (settingsOwner is null) AppWindow.Resize(new Windows.Graphics.SizeInt32(720, 760));
        UiText.LanguageChanged += LanguageChanged;
        AppWindow.Closing += WindowClosing;
        Closed += WindowClosed;
    }

    public bool HasPendingChanges => _busy;
    public Guid? ProjectId => _projectId;
    public bool IsEmbedded => _settingsOwner is not null;

    /// <summary>
    /// Reuses retrieval controls and polling in the settings content area, owned by the visible window.
    /// 在设置内容区复用检索控件和轮询，选择器与确认框由可见设置窗口持有。
    /// </summary>
    public FrameworkElement AttachToSettings(Window owner)
    {
        ArgumentNullException.ThrowIfNull(owner);
        ObjectDisposedException.ThrowIf(_closed, this);
        _settingsOwner = owner;
        SetTitleBar(null);
        _titleBar.Visibility = Visibility.Collapsed;
        _settingsRoot.RowDefinitions[0].Height = new GridLength(0);
        Content = null;
        return _settingsRoot;
    }

    private XamlRoot? DialogRoot => (_settingsOwner?.Content as FrameworkElement)?.XamlRoot ?? _settingsRoot.XamlRoot;

    public void DisposeEmbedded()
    {
        if (_closed) return;
        _allowClose = true;
        ReleaseResources();
        Close();
        _settingsOwner = null;
    }

    public void CloseForOwner()
    {
        if (IsEmbedded)
        {
            DisposeEmbedded();
            return;
        }
        _allowClose = true;
        Close();
    }

    private void WindowClosing(AppWindow sender, AppWindowClosingEventArgs args)
    {
        if (_busy && !_allowClose) args.Cancel = true;
    }

    private void WindowClosed(object sender, WindowEventArgs args) => ReleaseResources();

    private void ReleaseResources()
    {
        if (_closed) return;
        _closed = true;
        StopStatusObservation();
        _lifetime.Cancel();
        UiText.LanguageChanged -= LanguageChanged;
        AppWindow.Closing -= WindowClosing;
        Closed -= WindowClosed;
        if (_ownsApi && _api is IDisposable disposable) disposable.Dispose();
        _lifetime.Dispose();
    }

    private void LanguageChanged(object? sender, EventArgs args)
    {
        if (_closed) return;
        Title = UiText.Get("KYNXA · 检索与网页搜索");
        RefreshPickerSelections();
        // The last status snapshot can predate job polling; language changes must retain the newer observation.
        // 状态快照可能早于任务轮询；切换语言时保留较新的任务观察，不退回旧状态。
        var currentJob = _job;
        if (_status is { } status) RenderStatus(status);
        if (currentJob is { } job) ShowJob(job);
        if (_notice.IsOpen && _noticeMessageKey is { } messageKey)
            _notice.Message = UiText.Get(messageKey);
    }

    private void RefreshPickerSelections()
    {
        // WinUI caches a string selection box independently of ComboBoxItem.Content.
        // WinUI 的收起显示缓存独立于 ComboBoxItem.Content；重新选择同一项刷新显示，但不能触发保存。
        bool wasRendering = _rendering;
        _rendering = true;
        try
        {
            foreach (var picker in new[] { _localMode, _semanticMode, _rerankMode, _webMode, _webProvider, _webDepth, _webLanguage })
            {
                if (picker.SelectedItem is not { } selected) continue;
                picker.SelectedItem = null;
                picker.SelectedItem = selected;
            }
        }
        finally { _rendering = wasRendering; }
    }

    private async Task LoadAsync()
    {
        var globalTask = _api.GetSettingsAsync(_lifetime.Token);
        var projectTask = _projectId is { } projectId ? _api.GetProjectSettingsAsync(projectId, _lifetime.Token) : null;
        var statusTask = _api.GetStatusAsync(_lifetime.Token);
        var providersTask = _api.GetProvidersAsync(_lifetime.Token);
        var sourcesTask = _api.GetSourcesAsync(_projectId, _lifetime.Token);
        var requests = new List<Task> { globalTask, statusTask, providersTask, sourcesTask };
        if (projectTask is not null) requests.Add(projectTask);
        await Task.WhenAll(requests);
        if (_closed) return;
        _global = await globalTask;
        _project = projectTask is null ? null : await projectTask;
        RenderSettings(await providersTask);
        _statusObservationFailed = false;
        RenderStatus(await statusTask);
        RenderSources((await sourcesTask).Sources);
    }

    private void RenderSettings(RetrievalProvidersResponse? providers = null)
    {
        if (_global is null) return;
        _rendering = true;
        try
        {
            var local = _project?.Effective.Local ?? _global.Local;
            var web = _project?.Effective.Web ?? _global.Web;
            _inherit.IsChecked = _project?.Overrides is { Local: null, Web: null };
            Select(_localMode, local.Enabled ? "true" : "false");
            Select(_semanticMode, local.Semantic);
            if (local.RerankProfileId is { } profileId && !_rerankMode.Items.OfType<ComboBoxItem>().Any(item => Equals(item.Tag, profileId)))
                _rerankMode.Items.Add(new ComboBoxItem { Tag = profileId, Content = profileId });
            Select(_rerankMode, local.RerankProfileId ?? "off");
            if (providers is not null)
            {
                while (_webProvider.Items.Count > 1) _webProvider.Items.RemoveAt(1);
                foreach (var provider in providers.Providers.Where(provider => provider.Id != "auto"))
                    _webProvider.Items.Add(new ComboBoxItem { Tag = provider.Id, Content = provider.Name });
            }
            // Keep unavailable selected providers visible; do not silently replace the user's selection.
            // 保留当前不可用服务的选中项；不静默替换用户选择。
            if (!_webProvider.Items.OfType<ComboBoxItem>().Any(item => Equals(item.Tag, web.ProviderId)))
                _webProvider.Items.Add(new ComboBoxItem { Tag = web.ProviderId, Content = web.ProviderId });
            Select(_webMode, web.Mode);
            Select(_webProvider, web.ProviderId);
            Select(_webDepth, web.Depth);
            Select(_webLanguage, web.Language);
            bool inherits = _projectId is not null && _inherit.IsChecked == true;
            foreach (var picker in new[] { _localMode, _semanticMode, _rerankMode, _webMode, _webProvider, _webDepth, _webLanguage })
                picker.IsEnabled = !inherits;
        }
        finally { _rendering = false; }
    }

    private static void Select(ComboBox picker, string value)
    {
        picker.SelectedItem = picker.Items.OfType<ComboBoxItem>().FirstOrDefault(item => Equals(item.Tag, value));
    }

    private static string Value(ComboBox picker, string fallback) =>
        (picker.SelectedItem as ComboBoxItem)?.Tag as string ?? fallback;

    private async Task SaveAsync()
    {
        if (_busy || _global is null || _closed) return;
        var previousLocal = _project?.Effective.Local ?? _global.Local;
        var previousWeb = _project?.Effective.Web ?? _global.Web;
        string rerankProfileId = Value(_rerankMode, previousLocal.RerankProfileId ?? "off");
        var local = previousLocal with { Enabled = Value(_localMode, "true") == "true", Semantic = Value(_semanticMode, previousLocal.Semantic),
            RerankProfileId = rerankProfileId == "off" ? null : rerankProfileId };
        var web = previousWeb with { Mode = Value(_webMode, previousWeb.Mode), ProviderId = Value(_webProvider, previousWeb.ProviderId),
            Depth = Value(_webDepth, previousWeb.Depth), Language = Value(_webLanguage, previousWeb.Language) };
        bool inherits = _inherit.IsChecked == true;
        await RunOperationAsync(async () =>
        {
            try
            {
                if (_projectId is { } projectId && _project is { } project)
                {
                    var overrides = inherits ? new RetrievalSettingsOverrides() : new RetrievalSettingsOverrides(local, web);
                    var updated = await _api.SaveProjectSettingsAsync(projectId,
                        new(project.Revision, new(overrides)), _lifetime.Token);
                    if (!_closed) _project = updated;
                }
                else
                {
                    var updated = await _api.SaveSettingsAsync(new(_global.Revision, new(local, web)), _lifetime.Token);
                    if (!_closed) _global = updated;
                }
                if (!_closed) RenderSettings();
            }
            catch (GatewayApiException error) when (error.StatusCode == HttpStatusCode.Conflict)
            {
                // Refresh on conflict, never replay a stale form over concurrent changes.
                // 冲突时刷新；不把旧表单自动重放到并发更新上。
                await LoadAsync();
                ShowError("检索设置已在其他位置更新。已刷新，请重新选择。");
            }
            catch
            {
                if (!_closed) RenderSettings();
                throw;
            }
        });
    }

    private async Task ImportAsync(bool folder)
    {
        await RunOperationAsync(async () =>
        {
            string? path;
            if (folder)
            {
                var picker = new FolderPicker { SuggestedStartLocation = PickerLocationId.DocumentsLibrary };
                picker.FileTypeFilter.Add("*");
                WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(_settingsOwner ?? this));
                path = (await picker.PickSingleFolderAsync())?.Path;
            }
            else
            {
                var picker = new FileOpenPicker { SuggestedStartLocation = PickerLocationId.DocumentsLibrary };
                picker.FileTypeFilter.Add("*");
                WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(_settingsOwner ?? this));
                path = (await picker.PickSingleFileAsync())?.Path;
            }
            if (path is null || _closed) return;
            var imported = await _api.ImportSourceAsync(new(_projectId is null ? "user" : "project", path, _projectId), _lifetime.Token);
            if (_closed) return;
            RenderSources((await _api.GetSourcesAsync(_projectId, _lifetime.Token)).Sources);
            ShowJob(imported.JobId is { } jobId ? await _api.GetIndexJobAsync(jobId, _lifetime.Token) :
                await _api.RebuildIndexAsync(_projectId, _lifetime.Token));
        });
    }

    private void RenderSources(RetrievalSource[] sources)
    {
        _sourcesPanel.Children.Clear();
        // The retrieval API includes inherited global sources; manage each source only in its owning scope.
        // 检索接口包含继承的全局资料；管理操作只展示当前所属范围的资料。
        foreach (var source in sources.Where(source => _projectId is { } projectId ?
            source.Scope == "project" && source.ProjectId == projectId : source.Scope == "user"))
        {
            var label = new StackPanel { Spacing = 3 };
            label.Children.Add(new TextBlock { Text = source.Title, FontSize = 13, TextWrapping = TextWrapping.Wrap });
            label.Children.Add(new TextBlock { Text = source.Path, FontSize = 12, Opacity = 0.65, TextWrapping = TextWrapping.Wrap });
            var remove = ActionButton("移除资料", "RetrievalRemoveSource-" + source.Id);
            remove.Click += async (_, _) => await RunOperationAsync(async () =>
            {
                var choice = await DialogPresentation.ShowAsync(new ContentDialog { XamlRoot = DialogRoot,
                    Title = UiText.Get("移除检索资料？"), Content = UiText.Get("仅移除检索资料，不删除原文件。"),
                    PrimaryButtonText = UiText.Get("移除资料"), CloseButtonText = UiText.Get("取消"), DefaultButton = ContentDialogButton.Close });
                if (choice != ContentDialogResult.Primary || _closed) return;
                await _api.DeleteSourceAsync(source.Id, source.Revision, _lifetime.Token);
                if (_closed) return;
                RenderSources((await _api.GetSourcesAsync(_projectId, _lifetime.Token)).Sources);
            });
            var row = new Grid { ColumnSpacing = 10, Padding = new Thickness(14, 8, 14, 8) };
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            row.Children.Add(label);
            Grid.SetColumn(remove, 1);
            row.Children.Add(remove);
            _sourcesPanel.Children.Add(row);
        }
    }

    private void RenderStatus(RetrievalStatus status)
    {
        _status = status;
        RenderEmbeddingStatus(status.Embedding);
        _indexStatus.Text = string.Format(UiText.Get("{0} 个资料来源 · {1} 个索引片段"), status.SourceCount, status.ChunkCount);
        var scopedJobs = status.Jobs.Where(job => job.ProjectId == _projectId).ToArray();
        var latestJob = scopedJobs.LastOrDefault(job => IsActiveJob(job.Status)) ?? scopedJobs.LastOrDefault();
        if (latestJob is { } job) ShowJob(job);
        else
        {
            _job = null;
            _jobTimer.Stop();
            _cancelJob.Visibility = Visibility.Collapsed;
            _rebuild.IsEnabled = true;
            _rebuild.Visibility = Visibility.Collapsed;
        }
    }

    private void RenderEmbeddingStatus(RetrievalEmbeddingStatus embedding)
    {
        string summary = embedding.State switch
        {
            "ready" when embedding.Available && embedding.Loaded != false => UiText.Get("本地就绪"),
            "waiting" => UiText.Get("等待资源，使用关键词检索"),
            "loading" => UiText.Get("模型加载中，使用关键词检索"),
            "error" => UiText.Get("模型加载失败，使用关键词检索"),
            "unavailable" when embedding.Reason == "assets-missing" => UiText.Get("缺少检索模型文件，使用关键词检索"),
            "unavailable" => UiText.Get("模型不可用，使用关键词检索"),
            "unloaded" when embedding.Phase == "queued" => UiText.Get("模型未加载，后台预热排队中，使用关键词检索"),
            _ => UiText.Get("模型未加载，使用关键词检索")
        };
        if (embedding.State is "waiting" or "loading" && embedding.ElapsedMs is { } elapsedMs)
            summary += " · " + string.Format(UiText.Get("已用时 {0} 秒"), Math.Max(0, elapsedMs) / 1000);
        string? detail = embedding.Message ?? EmbeddingReason(embedding.Reason) ?? embedding.ErrorCode;
        if (!string.IsNullOrWhiteSpace(detail) && embedding.State != "ready")
            summary += "\n" + UiText.Get(detail);
        if (_statusObservationFailed)
            summary += "\n" + UiText.Get("状态暂时无法刷新，将自动重试");
        _embeddingStatus.Text = summary;
    }

    private static string? EmbeddingReason(string? reason) => reason switch
    {
        "startup-delay" => "启动后在后台准备，不阻塞聊天",
        "resource-admission" => "正在等待后台资源许可",
        "resource-busy" => "前台任务优先，资源空闲后自动继续",
        "model-loading" => "正在载入并验证检索模型",
        "assets-missing" => "请检查模型文件是否完整",
        "disabled" => "本地检索已停用",
        "semantic-off" => "语义检索已停用",
        "unsupported-profile" => "当前检索模型不支持本地预热",
        "load-failed" => "检索模型未能完成加载",
        _ => reason
    };

    private void StartStatusObservation()
    {
        if (_closed || _observation is not null) return;
        _observation = CancellationTokenSource.CreateLinkedTokenSource(_lifetime.Token);
        _statusTimer.Start();
        if (_job is { } job && IsActiveJob(job.Status)) _jobTimer.Start();
    }

    private void StopStatusObservation()
    {
        _statusTimer.Stop();
        _jobTimer.Stop();
        var observation = _observation;
        _observation = null;
        observation?.Cancel();
        observation?.Dispose();
    }

    private async Task PollStatusAsync()
    {
        if (_closed || _statusPolling || _busy || _observation is not { } observation) return;
        _statusPolling = true;
        try
        {
            var status = await _api.GetStatusAsync(observation.Token);
            if (_closed || _busy || !ReferenceEquals(_observation, observation)) return;
            // Observe only model residency; do not reload form values or replace a newer job receipt.
            // 只观察模型驻留，不重载表单，也不以旧快照覆盖较新的索引任务回执。
            _status = _status is { } previous ? previous with { Embedding = status.Embedding } : status;
            _statusObservationFailed = false;
            RenderEmbeddingStatus(status.Embedding);
        }
        catch (OperationCanceledException) when (_closed || !ReferenceEquals(_observation, observation)) { }
        catch (Exception)
        {
            // A transient observation failure is not evidence that the model failed to load.
            // 短暂状态读取失败不能被当成模型加载失败；保留最后回执，并在下次计时重试。
            if (!_closed && ReferenceEquals(_observation, observation))
            {
                _statusObservationFailed = true;
                if (_status is { } status) RenderEmbeddingStatus(status.Embedding);
                else _embeddingStatus.Text = UiText.Get("状态暂时无法刷新，将自动重试");
            }
        }
        finally { _statusPolling = false; }
    }

    private static bool IsActiveJob(string status) => status is "queued" or "running" or "paused";

    private void ShowJob(RetrievalIndexJob job)
    {
        if (_closed) return;
        _job = job;
        // Paused checkpoints remain owned by the gateway; polling can observe recovery, but UI never resumes them.
        // 暂停检查点仍归网关所有；轮询只观察恢复，界面不自行恢复或重复创建任务。
        bool active = IsActiveJob(job.Status);
        _cancelJob.Visibility = active ? Visibility.Visible : Visibility.Collapsed;
        _rebuild.IsEnabled = !active;
        // Normal work needs no indexing action; only an interrupted or incomplete job exposes recovery.
        // 正常工作无需索引操作；只有中断或未完整完成的任务才显示恢复入口。
        _rebuild.Visibility = job.Error != "INDEX_SUPERSEDED" && (job.Status is "cancelled" or "failed" or "partial")
            ? Visibility.Visible : Visibility.Collapsed;
        _indexStatus.Text = job.Status switch
        {
            "queued" => UiText.Get("索引任务等待中"),
            "running" => string.Format(UiText.Get("正在更新索引 · {0}/{1}"), job.CompletedSources, job.TotalSources),
            "paused" => UiText.Get("索引任务已暂停，等待恢复"),
            "completed" => UiText.Get("索引已更新"),
            "partial" => PartialJobSummary(job),
            "cancelled" when job.Error == "INDEX_SUPERSEDED" => UiText.Get("工作目录已变更，旧资料任务已停止"),
            "cancelled" => UiText.Get("索引任务已取消"),
            _ => UiText.Get("索引更新失败，请重试")
        };
        if (active && _observation is not null) _jobTimer.Start();
        else _jobTimer.Stop();
        if (job.Status == "failed") ShowError(job.Error ?? "索引更新失败，请重试");
    }

    private static string PartialJobSummary(RetrievalIndexJob job)
    {
        // Processed sources do not prove complete lexical or semantic coverage; display the actual receipt separately.
        // 已处理来源不代表关键词或语义覆盖完整；有覆盖回执时分别显示真实数量。
        if (job.Coverage is not { } coverage)
            return string.Format(UiText.Get("索引部分完成 · 已处理 {0}/{1} 个来源"), job.CompletedSources, job.TotalSources);
        return UiText.Get("索引部分完成") + "\n" + string.Format(
            UiText.Get("来源 {0} · 关键词覆盖 {1} · 语义覆盖 {2} · 失败 {3} · 跳过 {4} · 部分 {5}"),
            coverage.Discovered, coverage.Lexical, coverage.Semantic, coverage.Failed, coverage.Skipped, coverage.Partial);
    }

    private async Task PollJobAsync()
    {
        if (_closed || _polling || _busy || _job is not { } job || _observation is not { } observation) return;
        _polling = true;
        try
        {
            var updated = await _api.GetIndexJobAsync(job.JobId, observation.Token);
            if (_closed || !ReferenceEquals(_observation, observation) || _job?.JobId != job.JobId) return;
            ShowJob(updated);
        }
        catch (OperationCanceledException) when (_closed || !ReferenceEquals(_observation, observation)) { }
        catch (Exception error)
        {
            if (!_closed && ReferenceEquals(_observation, observation)) { _jobTimer.Stop(); ShowError(error.Message); }
        }
        finally { _polling = false; }
    }

    private async Task RunOperationAsync(Func<Task> operation)
    {
        if (_busy || _closed || StoragePaths.IsMigrating) return;
        _busy = true;
        _notice.IsOpen = false;
        _noticeMessageKey = null;
        _settingsHost.IsEnabled = false;
        try { await operation(); }
        catch (OperationCanceledException) when (_closed) { }
        catch (Exception error) { if (!_closed) ShowError(error.Message); }
        finally
        {
            _busy = false;
            if (!_closed) _settingsHost.IsEnabled = true;
        }
    }

    private void ShowError(string message)
    {
        // Keep the source key for an already visible local notice; unknown gateway details stay unchanged.
        // 保留已显示本地提示的原始键，支持即时换语言；未识别的网关详情仍按原文显示。
        _noticeMessageKey = message;
        _notice.Message = UiText.Get(message);
        _notice.Severity = InfoBarSeverity.Error;
        _notice.IsOpen = true;
    }
}
