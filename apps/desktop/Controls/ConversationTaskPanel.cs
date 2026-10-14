using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Markup;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Lazy, virtualized task projections. Hiding or disposing this view cancels reads, never work.
/// 延迟加载的虚拟任务投影；隐藏或释放视图只取消读取，绝不取消正式工作。
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
    private readonly HashSet<Guid> _controlling = [];
    private CancellationTokenSource _reads = new();
    private ITasksApi? _api;
    private Guid? _conversationId, _projectId, _nextTaskId;
    private bool _visible, _loading, _disposed;
    private long _generation;
    private string? _noticeKey;
    private DateTimeOffset _lastRefresh;

    public ConversationTaskPanel()
    {
        var body = new StackPanel { Spacing = 6 };
        var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        actions.Children.Add(_refresh); actions.Children.Add(_more);
        body.Children.Add(actions); body.Children.Add(_notice); body.Children.Add(_list);
        _section.Content = body; Children.Add(_section);
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
            UpdatePolling();
            if (_section.IsExpanded) _ = RefreshAsync();
        });
        _refresh.Click += (_, _) => _ = RefreshAsync();
        _more.Click += (_, _) => _ = RefreshAsync(loadMore: true);
        _poll.Tick += (_, _) => _ = RefreshAsync();
        UiText.LanguageChanged += LanguageChanged;
        RefreshLanguage();
    }

    public void ConfigureApi(ITasksApi api) => _api = api;
    public event EventHandler<TaskControlResponse>? ControlApplied;

    public void ShowConversation(Guid? conversationId, Guid? projectId)
    {
        if (_disposed || (_conversationId == conversationId && _projectId == projectId)) return;
        CancelReads(); _conversationId = conversationId; _projectId = projectId;
        _rows.Clear(); _realized.Clear(); _expanded.Clear(); _nextTaskId = null;
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
        _loading = false; _loadingDetails.Clear(); _controlling.Clear();
    }

    private void UpdatePolling()
    {
        if (CanRead) _poll.Start(); else _poll.Stop();
        _more.IsEnabled = !_loading && _nextTaskId is not null && _rows.Count < MaximumVisibleRoots;
    }

    private async Task RefreshAsync(bool loadMore = false)
    {
        if (!CanRead || _loading || (loadMore && _nextTaskId is null)) return;
        _loading = true;
        _lastRefresh = DateTimeOffset.UtcNow;
        long generation = _generation;
        Guid conversation = _conversationId!.Value;
        Guid? project = _projectId;
        var token = _reads.Token;
        try
        {
            var page = await _api!.ListAsync(conversation, project, loadMore ? _nextTaskId : null, token);
            if (!Current(generation)) return;
            if (loadMore) _nextTaskId = page.HasMore ? page.NextTaskId : null;
            else if (_rows.Count <= page.Tasks.Length) _nextTaskId = page.HasMore ? page.NextTaskId : null;
            // Refresh the first page in place; retained later pages keep their order and expanded controls.
            // 就地刷新第一页；已加载后续页保留顺序及展开状态，避免每次轮询重建整列。
            foreach (var task in page.Tasks.Where(task => task.ParentTaskId is null))
            {
                var row = _rows.FirstOrDefault(item => item.Record.TaskId == task.TaskId);
                if (task.DisplayInTaskList == false)
                {
                    if (row is not null) _rows.Remove(row);
                    _expanded.Remove(task.TaskId);
                    continue;
                }
                if (row is null)
                {
                    if (_rows.Count >= MaximumVisibleRoots) break;
                    _rows.Add(new(task));
                }
                else if (row.Record.Version != task.Version || row.Record.EventSequence != task.EventSequence)
                {
                    row.Update(task);
                }
            }
            foreach (var pair in _realized.Where(pair => _expanded.Contains(pair.Key)).ToArray())
                if (_rows.FirstOrDefault(row => row.Record.TaskId == pair.Key) is { } row) _ = LoadDetailsAsync(row, pair.Value);
            Notice(_rows.Count == 0 ? "此聊天暂无任务。" : _rows.Count >= MaximumVisibleRoots
                ? "已显示最近 500 个任务，更早记录仍保存在任务服务。" : null);
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
        if (!CanRead || !_expanded.Contains(taskId) || !_loadingDetails.Add(taskId)) return;
        long generation = _generation;
        var token = _reads.Token;
        try
        {
            var detail = await _api!.GetAsync(row.Record.Scope.ConversationId, taskId, _projectId, token);
            if (!Current(generation) || !_expanded.Contains(taskId) || expander.DataContext != row) return;
            row.Update(detail.Task);
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
        var controls = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 4 };
        if (!TaskStates.IsTerminal(task.State))
        {
            if (task.State != "paused") AddControl(controls, task, "pause", "暂停", generation);
            if (TaskStates.CanResume(task.State)) AddControl(controls, task, "resume", "继续执行", generation);
            AddControl(controls, task, "cancel", "取消任务", generation);
        }
        body.Children.Add(controls);
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
        if (detail.Operations.Length > 0)
        {
            body.Children.Add(Label(UiText.Get("操作回执")));
            body.Children.Add(new ListView { SelectionMode = ListViewSelectionMode.None, MaxHeight = 140,
                ItemsSource = detail.Operations.Select(operation => $"{operation.Intent.ToolName} · {OperationText(operation.State)}").ToArray() });
        }
        if (detail.Artifacts.Length > 0)
        {
            body.Children.Add(Label(UiText.Get("结果引用")));
            body.Children.Add(new ListView { SelectionMode = ListViewSelectionMode.None, MaxHeight = 150,
                ItemsSource = detail.Artifacts.Select(artifact => $"{artifact.Kind} · {artifact.ArtifactId}\n{ReferenceText(artifact.Reference)}").ToArray() });
        }
    }

    private void AddControl(StackPanel host, TaskRecord task, string action, string caption, long generation)
    {
        var button = new Button { Content = UiText.Get(caption), MinHeight = 26, Padding = new Thickness(6, 3, 6, 3) };
        button.Click += async (_, _) =>
        {
            if (!Current(generation) || !_controlling.Add(task.TaskId)) return;
            foreach (var item in host.Children.OfType<Button>()) item.IsEnabled = false;
            var token = _reads.Token;
            try
            {
                // Retry only after a fresh read and another explicit action; never replay a stale control.
                // 先刷新状态再由用户重新操作；不自动重放旧版本控制。
                var receipt = await _api!.ControlAsync(task.Scope.ConversationId, task.TaskId, task.Version, action, _projectId, token);
                ControlApplied?.Invoke(this, receipt);
                if (Current(generation)) await RefreshAsync();
                if (Current(generation) && _realized.TryGetValue(task.TaskId, out var expander)
                    && _rows.FirstOrDefault(row => row.Record.TaskId == task.TaskId) is { } row)
                    await LoadDetailsAsync(row, expander);
            }
            catch (OperationCanceledException) when (token.IsCancellationRequested) { }
            catch (Exception error) when (error is HttpRequestException or InvalidOperationException or InvalidDataException or JsonException or TaskCanceledException)
            {
                if (Current(generation))
                {
                    Notice("任务控制未确认，请刷新状态后再操作。");
                    await RefreshAsync();
                }
            }
            finally
            {
                if (_generation == generation)
                {
                    _controlling.Remove(task.TaskId);
                    foreach (var item in host.Children.OfType<Button>()) item.IsEnabled = true;
                }
            }
        };
        host.Children.Add(button);
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
    private static string OperationText(string state) => UiText.Get(state switch
    { "pending" => "等待执行", "dispatched" => "正在执行", "succeeded" => "执行完成", "failed" => "执行失败", "unknown" => "执行结果未知", _ => state });
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
        public string Goal => Record.Goal;
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
}
