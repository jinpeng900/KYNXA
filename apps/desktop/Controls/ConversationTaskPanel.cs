using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Markup;

namespace KYNXA_Desktop.Controls;

public sealed record TaskArtifactRequest(Guid ConversationId, TaskArtifact Artifact);

/// <summary>
/// Read-only, virtualized task projections. Hiding or disposing cancels reads, never work.
/// 只读的虚拟任务投影；隐藏或释放只取消读取，绝不改变正式工作。
/// </summary>
public sealed class ConversationTaskPanel : Grid, IDisposable
{
    private const int MaximumVisibleRoots = 500;
    private readonly ObservableCollection<TaskRow> _rows = [];
    private readonly ListView _list = new() { SelectionMode = ListViewSelectionMode.None, MaxHeight = 320 };
    private readonly Expander _section = new() { HorizontalAlignment = HorizontalAlignment.Stretch };
    private readonly TextBlock _notice = new() { FontSize = 12, TextWrapping = TextWrapping.Wrap };
    private readonly Button _refresh = new() { MinHeight = 28, Padding = new Thickness(8, 3, 8, 3) };
    private readonly Button _more = new() { MinHeight = 28, Padding = new Thickness(8, 3, 8, 3) };
    private readonly DispatcherTimer _poll = new() { Interval = TimeSpan.FromSeconds(4) };
    private readonly Dictionary<Guid, Expander> _realized = [];
    private readonly HashSet<Guid> _expanded = [];
    private readonly HashSet<Guid> _loadingDetails = [];
    private readonly HashSet<Guid> _recoverableTaskIds = [];
    private CancellationTokenSource _reads = new();
    private ITasksApi? _api;
    private Guid? _conversationId, _projectId, _nextTaskId, _nextRecoverableTaskId;
    private bool _visible, _loading, _disposed;
    private long _generation;
    private string? _noticeKey;
    private DateTimeOffset _lastRefresh;
    private readonly Grid _body = new() { RowSpacing = 6 };
    private double _availableHeight = 220;

    public bool HasTasks => _rows.Count > 0;
    public bool IsExpanded => _section.IsExpanded;
    public event EventHandler? TasksChanged;
    public event EventHandler<TaskArtifactRequest>? ArtifactOpenRequested;
    public event EventHandler<TaskArtifactRequest[]>? ArtifactsChanged;

    public ConversationTaskPanel()
    {
        _body.RowDefinitions.Add(new() { Height = GridLength.Auto });
        _body.RowDefinitions.Add(new() { Height = GridLength.Auto });
        _body.RowDefinitions.Add(new() { Height = new GridLength(1, GridUnitType.Star) });
        var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        actions.Children.Add(_refresh); actions.Children.Add(_more);
        _body.Children.Add(actions); Grid.SetRow(_notice, 1); _body.Children.Add(_notice);
        Grid.SetRow(_list, 2); _body.Children.Add(_list);
        _list.MaxHeight = double.PositiveInfinity;
        _section.Content = _body; _section.IsExpanded = false;
        _section.MinHeight = 28; _section.FontSize = 12;
        _section.Resources["ExpanderHeaderPadding"] = new Thickness(8, 0, 8, 0);
        _section.Resources["ExpanderChevronButtonSize"] = 24d;
        _section.Resources["ExpanderChevronMargin"] = new Thickness(8, 0, 4, 0);
        Children.Add(_section);
        _list.ItemsSource = _rows;
        _list.ItemTemplate = (DataTemplate)XamlReader.Load("""
            <DataTemplate xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation">
                <Expander HorizontalAlignment="Stretch" HorizontalContentAlignment="Stretch">
                    <Expander.Header>
                        <StackPanel Spacing="3">
                            <TextBlock Text="{Binding Goal}" FontSize="12" TextTrimming="CharacterEllipsis" MaxWidth="360" />
                            <TextBlock Text="{Binding StateText}" FontSize="11" />
                        </StackPanel>
                    </Expander.Header>
                    <StackPanel Spacing="6" />
                </Expander>
            </DataTemplate>
            """);
        _list.ContainerContentChanging += ContainerChanged;
        _section.RegisterPropertyChangedCallback(Expander.IsExpandedProperty, (_, _) =>
        {
            if (!_section.IsExpanded) CancelReads();
            UpdateBodyHeight();
            UpdatePolling();
            if (_section.IsExpanded) _ = RefreshAsync();
            TasksChanged?.Invoke(this, EventArgs.Empty);
        });
        _refresh.Click += (_, _) => _ = RefreshAsync();
        _more.Click += (_, _) => _ = RefreshAsync(loadMore: true);
        _poll.Tick += (_, _) => _ = RefreshAsync();
        UiText.LanguageChanged += LanguageChanged;
        RefreshLanguage();
        UpdateBodyHeight();
    }

    public void SetAvailableHeight(double height)
    {
        _availableHeight = Math.Max(72, height);
        MaxHeight = _availableHeight;
        UpdateBodyHeight();
    }

    private void UpdateBodyHeight()
    {
        _body.Height = Math.Max(32, _availableHeight - 32);
        // Explicit collapsed geometry wins over the framework's default expander header height.
        // 收起时使用明确高度，避免框架默认 Expander 标题仍占据大块空间。
        _section.Height = _section.IsExpanded ? double.NaN : 28;
        _section.MaxHeight = _section.IsExpanded ? _availableHeight : 28;
    }

    public void ConfigureApi(ITasksApi api) => _api = api;

    public void ShowConversation(Guid? conversationId, Guid? projectId)
    {
        if (_disposed || (_conversationId == conversationId && _projectId == projectId)) return;
        CancelReads(); _conversationId = conversationId; _projectId = projectId;
        _rows.Clear(); _realized.Clear(); _expanded.Clear(); _nextTaskId = null; _nextRecoverableTaskId = null;
        _recoverableTaskIds.Clear();
        TasksChanged?.Invoke(this, EventArgs.Empty);
        _noticeKey = null; _notice.Text = ""; _section.IsEnabled = conversationId is not null;
        UpdatePolling();
        if (CanRead) _ = RefreshAsync();
    }

    public void SetPreviewEnabled(bool enabled)
    {
        if (_disposed || _visible == enabled) return;
        _visible = enabled;
        if (!enabled) CancelReads();
        UpdatePolling();
        if (CanRead) _ = RefreshAsync();
    }

    public void NotifyChanged(Guid conversationId)
    {
        if (CanRead && _conversationId == conversationId && DateTimeOffset.UtcNow - _lastRefresh >= TimeSpan.FromSeconds(1))
            _ = RefreshAsync();
    }

    private bool CanRead => !_disposed && _visible && _section.IsExpanded && _conversationId is not null && _api is not null;

    private void CancelReads()
    {
        _generation++; _reads.Cancel(); _reads.Dispose(); _reads = new();
        _loading = false; _loadingDetails.Clear();
    }

    private void UpdatePolling()
    {
        if (CanRead) _poll.Start(); else _poll.Stop();
        _more.IsEnabled = !_loading && (_nextRecoverableTaskId is not null ||
            _nextTaskId is not null && _rows.Count(row => !_recoverableTaskIds.Contains(row.Record.TaskId)) < MaximumVisibleRoots);
    }

    private async Task RefreshAsync(bool loadMore = false)
    {
        if (!CanRead || _loading || (loadMore && _nextTaskId is null && _nextRecoverableTaskId is null)) return;
        _loading = true;
        _lastRefresh = DateTimeOffset.UtcNow;
        long generation = _generation;
        Guid conversation = _conversationId!.Value;
        Guid? project = _projectId;
        var token = _reads.Token;
        try
        {
            var normalRead = !loadMore || _nextTaskId is not null
                ? _api!.ListAsync(conversation, project, loadMore ? _nextTaskId : null, token)
                : Task.FromResult(new TaskListResponse([], false, null));
            var recoveryRead = !loadMore || _nextRecoverableTaskId is not null
                ? _api!.RecoverableAsync(conversation, project, loadMore ? _nextRecoverableTaskId : null, token)
                : Task.FromResult(new TaskListResponse([], false, null));
            await Task.WhenAll(normalRead, recoveryRead);
            var page = await normalRead;
            var recoverable = await recoveryRead;
            if (!Current(generation)) return;
            if (loadMore) _nextTaskId = page.HasMore ? page.NextTaskId : null;
            else if (_nextTaskId is null || _rows.Count(row => !_recoverableTaskIds.Contains(row.Record.TaskId)) <= page.Tasks.Length)
                _nextTaskId = page.HasMore ? page.NextTaskId : null;
            if (loadMore || _nextRecoverableTaskId is null || _recoverableTaskIds.Count <= recoverable.Tasks.Length)
                _nextRecoverableTaskId = recoverable.HasMore ? recoverable.NextTaskId : null;
            if (!loadMore && !recoverable.HasMore)
            {
                var currentRoots = recoverable.Tasks.Select(task => task.TaskId).ToHashSet();
                foreach (var row in _rows.Where(row => _recoverableTaskIds.Contains(row.Record.TaskId) &&
                    !currentRoots.Contains(row.Record.TaskId) && !page.Tasks.Any(task => task.TaskId == row.Record.TaskId)).ToArray())
                    _rows.Remove(row);
                _recoverableTaskIds.IntersectWith(currentRoots);
            }
            // Recoverable roots remain visible in this chat without selecting or controlling execution.
            // 当前聊天的可恢复根任务仍可查看，不选择执行目标，也不改变任务状态。
            int recoveryPosition = loadMore ? _rows.Count(row => _recoverableTaskIds.Contains(row.Record.TaskId)) : 0;
            foreach (var task in recoverable.Tasks)
            {
                _recoverableTaskIds.Add(task.TaskId);
                var row = _rows.FirstOrDefault(item => item.Record.TaskId == task.TaskId);
                if (row is null) _rows.Insert(recoveryPosition, new(task));
                else
                {
                    row.Update(task);
                    int previousPosition = _rows.IndexOf(row);
                    if (!loadMore && previousPosition != recoveryPosition) _rows.Move(previousPosition, recoveryPosition);
                }
                if (!loadMore || row is null) recoveryPosition++;
            }
            // Refresh the first page in place; retained later pages keep their order and expanded controls.
            // 就地刷新第一页；已加载后续页保留顺序及展开状态，避免每次轮询重建整列。
            foreach (var task in page.Tasks.Where(task => task.ParentTaskId is null))
            {
                var row = _rows.FirstOrDefault(item => item.Record.TaskId == task.TaskId);
                if (task.DisplayInTaskList == false && !_recoverableTaskIds.Contains(task.TaskId))
                {
                    if (row is not null) _rows.Remove(row);
                    _expanded.Remove(task.TaskId);
                    continue;
                }
                if (row is null)
                {
                    if (_rows.Count(item => !_recoverableTaskIds.Contains(item.Record.TaskId)) >= MaximumVisibleRoots) break;
                    _rows.Add(new(task));
                }
                else if (row.Record.Version != task.Version || row.Record.EventSequence != task.EventSequence)
                {
                    row.Update(task);
                }
            }
            if (_section.IsExpanded)
                foreach (var pair in _realized.Where(pair => _expanded.Contains(pair.Key)).ToArray())
                    if (_rows.FirstOrDefault(row => row.Record.TaskId == pair.Key) is { } row) _ = LoadDetailsAsync(row, pair.Value);
            Notice(_rows.Count == 0 ? "此聊天暂无任务。" : _rows.Count(row => !_recoverableTaskIds.Contains(row.Record.TaskId)) >= MaximumVisibleRoots
                ? "已显示最近 500 个任务，更早记录仍保存在任务服务。" : null);
            TasksChanged?.Invoke(this, EventArgs.Empty);
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested) { }
        catch (Exception error) when (error is HttpRequestException or InvalidOperationException or InvalidDataException or JsonException or TaskCanceledException)
        {
            if (Current(generation)) Notice("任务状态读取失败；聊天仍可继续。");
        }
        finally
        {
            if (Current(generation)) { _loading = false; UpdatePolling(); }
        }
    }

    private bool Current(long generation) => CanRead && _generation == generation;

    private void ContainerChanged(ListViewBase sender, ContainerContentChangingEventArgs args)
    {
        if (args.ItemContainer.ContentTemplateRoot is not Expander expander) return;
        if (args.InRecycleQueue)
        {
            foreach (var key in _realized.Where(pair => pair.Value == expander).Select(pair => pair.Key).ToArray()) _realized.Remove(key);
            if (expander.Tag is RowHook recycled) recycled.Recycling = true;
            expander.IsExpanded = false;
            if (expander.Content is StackPanel oldBody) oldBody.Children.Clear();
            return;
        }
        if (args.Item is not TaskRow row) return;
        if (expander.Tag is null)
        {
            expander.Tag = new RowHook();
            expander.RegisterPropertyChangedCallback(Expander.IsExpandedProperty, (_, _) =>
            {
                if (expander.Tag is RowHook { Recycling: false } && expander.DataContext is TaskRow active)
                {
                    if (expander.IsExpanded) { _expanded.Add(active.Record.TaskId); _ = LoadDetailsAsync(active, expander); }
                    else _expanded.Remove(active.Record.TaskId);
                }
            });
        }
        if (expander.Tag is RowHook hook) hook.Recycling = false;
        _realized[row.Record.TaskId] = expander;
        expander.IsExpanded = _expanded.Contains(row.Record.TaskId);
    }

    private async Task LoadDetailsAsync(TaskRow row, Expander expander)
    {
        Guid taskId = row.Record.TaskId;
        if (!CanRead || !_section.IsExpanded || !_expanded.Contains(taskId) || !_loadingDetails.Add(taskId)) return;
        long generation = _generation;
        var token = _reads.Token;
        try
        {
            var detail = await _api!.GetAsync(row.Record.Scope.ConversationId, taskId, _projectId, token);
            if (!Current(generation) || !_section.IsExpanded || !_expanded.Contains(taskId) || expander.DataContext != row) return;
            row.Update(detail.Task);
            ArtifactsChanged?.Invoke(this, detail.Artifacts.Select(artifact => new TaskArtifactRequest(
                detail.Task.Scope.ConversationId, artifact)).ToArray());
            if (expander.Content is StackPanel body) RenderDetail(body, detail, generation);
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested) { }
        catch (Exception error) when (error is HttpRequestException or InvalidOperationException or InvalidDataException or JsonException or TaskCanceledException)
        {
            if (Current(generation)) Notice("任务状态读取失败；聊天仍可继续。");
        }
        finally { if (_generation == generation) _loadingDetails.Remove(taskId); }
    }

    private void RenderDetail(StackPanel body, TaskDetailResponse detail, long generation)
    {
        body.Children.Clear();
        var task = detail.Task;
        var usage = task.Usage;
        body.Children.Add(Label(string.Format(UiText.Get("模型调用 {0} · 重试 {1} · 压缩 {2}"),
            usage.ModelRequests, usage.Retries, usage.Compactions)));
        body.Children.Add(Label(string.Format(UiText.Get("输入 {0} · 输出 {1} tokens · 未知用量 {2} 次"),
            usage.InputTokens, usage.OutputTokens, usage.UnknownTokenRequests)));
        body.Children.Add(Label(string.Format(UiText.Get("模型耗时 {0:F1}s · 工具耗时 {1:F1}s · 失败调用 {2}"),
            usage.ModelDurationMs / 1000d, usage.ToolDurationMs / 1000d, usage.FailedRequests)));
        if (!string.IsNullOrEmpty(task.StopReason)) body.Children.Add(Label(task.StopReason));
        if (detail.Children.Length > 0)
        {
            body.Children.Add(Label(UiText.Get("子任务")));
            var children = new ListView { SelectionMode = ListViewSelectionMode.None, MaxHeight = 170,
                IsItemClickEnabled = true, DisplayMemberPath = nameof(TaskRow.Goal),
                ItemsSource = detail.Children.Select(child => new TaskRow(child)).ToArray() };
            var selectedChild = new StackPanel { Spacing = 6 };
            long selection = 0;
            children.ItemClick += async (_, args) =>
            {
                if (args.ClickedItem is not TaskRow child || !Current(generation)) return;
                long selected = ++selection;
                var token = _reads.Token;
                try
                {
                    var childDetail = await _api!.GetAsync(child.Record.Scope.ConversationId, child.Record.TaskId, _projectId, token);
                    if (!Current(generation) || selected != selection || !children.IsLoaded) return;
                    RenderDetail(selectedChild, childDetail, generation);
                    selectedChild.Children.Insert(0, Label($"{StateText(childDetail.Task.State)} · {childDetail.Task.Goal}"));
                }
                catch (OperationCanceledException) when (token.IsCancellationRequested) { }
                catch (Exception error) when (error is HttpRequestException or InvalidOperationException or InvalidDataException or JsonException or TaskCanceledException)
                { if (Current(generation)) Notice("任务状态读取失败；聊天仍可继续。"); }
            };
            body.Children.Add(children);
            body.Children.Add(selectedChild);
        }
        if (detail.Artifacts.Length > 0)
        {
            body.Children.Add(Label(UiText.Get("结果引用")));
            var artifacts = new ListView { SelectionMode = ListViewSelectionMode.None, MaxHeight = 150,
                IsItemClickEnabled = true, DisplayMemberPath = nameof(ArtifactRow.Title),
                ItemsSource = detail.Artifacts.Select(artifact => new ArtifactRow(artifact,
                    $"{artifact.Kind} · {ReferenceText(artifact.Reference)}")).ToArray() };
            artifacts.ItemClick += (_, args) =>
            {
                if (Current(generation) && args.ClickedItem is ArtifactRow artifact)
                    ArtifactOpenRequested?.Invoke(this, new(task.Scope.ConversationId, artifact.Artifact));
            };
            body.Children.Add(artifacts);
        }
    }

    private static TextBlock Label(string text) => new() { Text = text, FontSize = 11, TextWrapping = TextWrapping.Wrap,
        IsTextSelectionEnabled = true };
    private static string ReferenceText(JsonElement reference)
    {
        if (reference.ValueKind != JsonValueKind.Object) return "";
        foreach (string key in new[] { "resultRef", "relativePath", "path", "url", "artifactId" })
            if (reference.TryGetProperty(key, out var value) && value.ValueKind == JsonValueKind.String)
            {
                string text = value.GetString() ?? "";
                return text.Length <= 400 ? text : text[..400] + "…";
            }
        return "";
    }
    internal static string StateText(string state) => UiText.Get(state switch
    {
        "queued" => "等待执行", "running" => "正在执行", "waiting-network" => "等待网络",
        "waiting-config" => "等待配置", "waiting-approval" => "等待批准", "waiting-verification" => "等待核验",
        "waiting-children" => "等待子任务", "paused" => "已暂停", "cancelled" => "已取消",
        "failed" => "执行失败", "completed" => "执行完成", "partial" => "已结束 · 有待办事项", _ => state
    });
    private void Notice(string? key) { _noticeKey = key; _notice.Text = key is null ? "" : UiText.Get(key); }
    private void RefreshLanguage()
    {
        _section.Header = UiText.Get("任务进度"); _refresh.Content = UiText.Get("刷新任务"); _more.Content = UiText.Get("更多任务");
        _notice.Text = _noticeKey is null ? "" : UiText.Get(_noticeKey);
        foreach (var row in _rows) row.RefreshLanguage();
    }
    private void LanguageChanged(object? sender, EventArgs args) => DispatcherQueue.TryEnqueue(() =>
    {
        if (_disposed) return;
        RefreshLanguage();
        foreach (var pair in _realized.Where(pair => _expanded.Contains(pair.Key)).ToArray())
            if (_rows.FirstOrDefault(row => row.Record.TaskId == pair.Key) is { } row) _ = LoadDetailsAsync(row, pair.Value);
    });
    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true; _poll.Stop(); _reads.Cancel(); _reads.Dispose();
        UiText.LanguageChanged -= LanguageChanged;
        _rows.Clear(); _realized.Clear(); _expanded.Clear();
    }

    public sealed class TaskRow(TaskRecord record) : INotifyPropertyChanged
    {
        public TaskRecord Record { get; private set; } = record;
        public string Goal => string.IsNullOrWhiteSpace(Record.Title) ? Record.Goal : Record.Title;
        public string StateText => ConversationTaskPanel.StateText(Record.State);
        public event PropertyChangedEventHandler? PropertyChanged;
        public void Update(TaskRecord updated)
        {
            Record = updated;
            PropertyChanged?.Invoke(this, new(nameof(Goal))); RefreshLanguage();
        }
        public void RefreshLanguage() => PropertyChanged?.Invoke(this, new(nameof(StateText)));
    }

    private sealed class RowHook { public bool Recycling { get; set; } }
    private sealed record ArtifactRow(TaskArtifact Artifact, string Title);
}
