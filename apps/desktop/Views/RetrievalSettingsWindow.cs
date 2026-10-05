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
    private RetrievalSettingsDocument? _global;
    private ProjectRetrievalSettingsDocument? _project;
    private RetrievalIndexJob? _job;
    private RetrievalStatus? _status;
    private bool _closed, _busy, _rendering, _loaded, _polling, _allowClose;

    public RetrievalSettingsWindow(Guid? projectId = null, string? projectName = null, string? mountedPath = null,
        IRetrievalApi? api = null)
    {
        if (projectId == Guid.Empty) throw new ArgumentException("A saved project ID is required.", nameof(projectId));
        _projectId = projectId;
        _projectName = projectName;
        _mountedPath = mountedPath;
        _api = api ?? new RetrievalApiClient();
        _ownsApi = api is null;
        BuildLayout();
        Title = UiText.Get("KYNXA · 检索与网页搜索");
        AppWindow.Resize(new Windows.Graphics.SizeInt32(720, 760));
        if (AppWindowTitleBar.IsCustomizationSupported())
        {
            AppWindow.TitleBar.ButtonBackgroundColor = Microsoft.UI.Colors.Transparent;
            AppWindow.TitleBar.ButtonInactiveBackgroundColor = Microsoft.UI.Colors.Transparent;
            AppWindow.TitleBar.ButtonForegroundColor = Microsoft.UI.Colors.Black;
        }
        UiText.LanguageChanged += LanguageChanged;
        AppWindow.Closing += (_, args) => { if (_busy && !_allowClose) args.Cancel = true; };
        Closed += (_, _) =>
        {
            _closed = true;
            _jobTimer.Stop();
            _lifetime.Cancel();
            UiText.LanguageChanged -= LanguageChanged;
            if (_ownsApi && _api is IDisposable disposable) disposable.Dispose();
            _lifetime.Dispose();
        };
    }

    public bool HasPendingChanges => _busy;
    public Guid? ProjectId => _projectId;

    public void CloseForOwner()
    {
        _allowClose = true;
        Close();
    }

    private void LanguageChanged(object? sender, EventArgs args)
    {
        if (_closed) return;
        Title = UiText.Get("KYNXA · 检索与网页搜索");
        if (_status is { } status) RenderStatus(status);
        if (_job is { } job) ShowJob(job);
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
            _mountedFolder.IsChecked = _project?.IndexingSources.MountedFolder.Enabled ?? false;
            _mountedFolder.IsEnabled = !string.IsNullOrWhiteSpace(_mountedPath);
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
        bool indexesMountedFolder = _mountedFolder.IsChecked == true;
        await RunOperationAsync(async () =>
        {
            try
            {
                if (_projectId is { } projectId && _project is { } project)
                {
                    var overrides = inherits ? new RetrievalSettingsOverrides() : new RetrievalSettingsOverrides(local, web);
                    var sources = new RetrievalIndexingSourcesPatch(new(indexesMountedFolder));
                    var updated = await _api.SaveProjectSettingsAsync(projectId,
                        new(project.Revision, new(overrides, sources)), _lifetime.Token);
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
                ShowError(UiText.Get("检索设置已在其他位置更新。已刷新，请重新选择。"));
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
                WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
                path = (await picker.PickSingleFolderAsync())?.Path;
            }
            else
            {
                var picker = new FileOpenPicker { SuggestedStartLocation = PickerLocationId.DocumentsLibrary };
                picker.FileTypeFilter.Add("*");
                WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
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
                var choice = await new ContentDialog { XamlRoot = ((FrameworkElement)Content).XamlRoot,
                    Title = UiText.Get("移除检索资料？"), Content = UiText.Get("仅移除检索资料，不删除原文件。"),
                    PrimaryButtonText = UiText.Get("移除资料"), CloseButtonText = UiText.Get("取消"), DefaultButton = ContentDialogButton.Close }.ShowAsync();
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
        _embeddingStatus.Text = status.Embedding.State switch
        {
            "ready" when status.Embedding.Available => UiText.Get("本地就绪"),
            "loading" => UiText.Get("模型加载中"),
            "error" => UiText.Get("模型不可用，使用关键词检索"),
            _ => UiText.Get("模型未就绪，使用关键词检索")
        };
        _indexStatus.Text = string.Format(UiText.Get("{0} 个资料来源 · {1} 个索引片段"), status.SourceCount, status.ChunkCount);
        if (status.Jobs.LastOrDefault(job => job.Status is "queued" or "running") is { } active) ShowJob(active);
    }

    private void ShowJob(RetrievalIndexJob job)
    {
        if (_closed) return;
        _job = job;
        bool active = job.Status is "queued" or "running";
        _cancelJob.Visibility = active ? Visibility.Visible : Visibility.Collapsed;
        _rebuild.IsEnabled = !active;
        _indexStatus.Text = job.Status switch
        {
            "queued" => UiText.Get("索引任务等待中"),
            "running" => string.Format(UiText.Get("正在更新索引 · {0}/{1}"), job.CompletedSources, job.TotalSources),
            "completed" => UiText.Get("索引已更新"),
            "cancelled" => UiText.Get("索引任务已取消"),
            _ => UiText.Get("索引更新失败，请重试")
        };
        if (active) _jobTimer.Start();
        else _jobTimer.Stop();
        if (job.Status == "failed") ShowError(job.Error ?? UiText.Get("索引更新失败，请重试"));
    }

    private async Task PollJobAsync()
    {
        if (_closed || _polling || _busy || _job is not { } job) return;
        _polling = true;
        try
        {
            var updated = await _api.GetIndexJobAsync(job.JobId, _lifetime.Token);
            if (_closed || _job?.JobId != job.JobId) return;
            ShowJob(updated);
        }
        catch (OperationCanceledException) when (_closed) { }
        catch (Exception error)
        {
            if (!_closed) { _jobTimer.Stop(); ShowError(error.Message); }
        }
        finally { _polling = false; }
    }

    private async Task RunOperationAsync(Func<Task> operation)
    {
        if (_busy || _closed || StoragePaths.IsMigrating) return;
        _busy = true;
        _notice.IsOpen = false;
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
        _notice.Message = message;
        _notice.Severity = InfoBarSeverity.Error;
        _notice.IsOpen = true;
    }
}
