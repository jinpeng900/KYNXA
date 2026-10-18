using System.Collections.ObjectModel;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop.Views;

/// <summary>
/// Manages local generation models inside settings; leaving the page never cancels downloads.
/// 在设置内管理本地生成模型；离开子页面仅停止界面轮询，不取消后台下载。
/// </summary>
public sealed partial class LocalModelsPage : UserControl, IDisposable
{
    private readonly Window _owner;
    private readonly LocalModelsApiClient _api = new();
    private readonly CancellationTokenSource _lifetime = new();
    private readonly ObservableCollection<ModelRow> _visibleRows = [];
    private readonly Dictionary<string, ModelRow> _rows = new(StringComparer.Ordinal);
    private readonly HashSet<string> _busyModels = new(StringComparer.Ordinal);
    private readonly DispatcherTimer _downloadTimer = new() { Interval = TimeSpan.FromSeconds(3) };
    private LocalModelsSnapshot? _snapshot;
    private string? _pickerSignature;
    private string? _storageDirectory;
    private long? _storageAvailableBytes;
    private DateTimeOffset _storageRefreshAt;
    private long _snapshotEpoch;
    private bool _closed, _loaded, _rendering, _polling, _settingsBusy;
    public event EventHandler? ModelsChanged;
    public event EventHandler? BackRequested;

    public LocalModelsPage(Window owner)
    {
        _owner = owner;
        BuildLayout();
        UiText.LanguageChanged += LanguageChanged;
        _downloadTimer.Tick += async (_, _) => await PollDownloadsAsync();
        Unloaded += (_, _) => Dispose();
    }

    public void Dispose()
    {
        if (_closed) return;
        _closed = true;
        _downloadTimer.Stop();
        _lifetime.Cancel();
        UiText.LanguageChanged -= LanguageChanged;
        _api.Dispose();
    }

    private async Task RefreshAsync()
    {
        if (_closed) return;
        long epoch = _snapshotEpoch;
        try
        {
            var snapshot = await _api.ReadAsync(_lifetime.Token);
            if (epoch == _snapshotEpoch) ApplySnapshot(snapshot);
        }
        catch (OperationCanceledException) when (_closed) { }
        catch (Exception error) { ShowError(error); }
    }

    private async Task PollDownloadsAsync()
    {
        if (_closed || _polling || _snapshot?.Models.Any(IsTransfer) != true) return;
        _polling = true;
        try { await RefreshAsync(); }
        finally { _polling = false; }
    }

    private void ApplySnapshot(LocalModelsSnapshot snapshot)
    {
        if (_closed) return;
        _snapshot = snapshot;
        _rendering = true;
        try
        {
            _enabled.IsOn = snapshot.Enabled;
            _directory.Text = snapshot.InstallDirectory;
            RenderStorageSpace();
            _ = RefreshStorageSpaceAsync(snapshot.InstallDirectory);
            _policy.SelectedItem = _policy.Items.OfType<ComboBoxItem>().FirstOrDefault(item => item.Tag as string == snapshot.LoadPolicy);
            foreach (var model in snapshot.Models)
            {
                if (!_rows.TryGetValue(model.Id, out var row)) _rows[model.Id] = row = new ModelRow(model);
                row.Apply(model, snapshot.Enabled, _busyModels.Contains(model.Id));
            }
            var ids = snapshot.Models.Select(model => model.Id).ToHashSet(StringComparer.Ordinal);
            foreach (string id in _rows.Keys.Where(id => !ids.Contains(id)).ToArray()) _rows.Remove(id);
            ApplyFilter();
            if (snapshot.Hardware is { } hardware && hardware.MemoryGb is > 0)
                _hardware.Text = string.Format(UiText.Get("本机内存 {0:0.#} GB · 可用 {1:0.#} GB · 显存 {2:0.#} GB · {3}"),
                    hardware.MemoryGb, hardware.AvailableMemoryGb ?? 0, hardware.VramGb ?? 0, hardware.GpuName ?? "");
            else _hardware.Text = UiText.Get("按模型体积、建议内存和显存选择；实际可用容量由资源服务动态判断。");
            _enabled.IsEnabled = !_settingsBusy;
            _policy.IsEnabled = !_settingsBusy;
            _changeDirectory.IsEnabled = !_settingsBusy;
            _import.IsEnabled = !_settingsBusy;
        }
        finally { _rendering = false; }
        if (snapshot.Models.Any(IsTransfer)) _downloadTimer.Start();
        else _downloadTimer.Stop();
        // Catalog changes refresh available choices, never replace the user's current API/model selection here.
        // 目录变化只刷新可选模型；此处不改用户当前 API 或模型选择。
        string signature = snapshot.Enabled + ":" + string.Join("|", snapshot.Models.Where(model => model.Installed && model.Enabled)
            .Select(model => model.Id).Order(StringComparer.Ordinal));
        bool changed = _pickerSignature is not null && _pickerSignature != signature;
        _pickerSignature = signature;
        if (changed) ModelsChanged?.Invoke(this, EventArgs.Empty);
    }

    private void ApplyFilter()
    {
        if (_closed) return;
        string filter = (_filter.SelectedItem as ComboBoxItem)?.Tag as string ?? "all";
        string query = _search.Text.Trim();
        var desired = (_snapshot?.Models ?? []).Where(model => filter switch
            { "installed" => model.Installed, "downloading" => IsTransfer(model) || model.Status == "paused", _ => true })
            .Where(model => query.Length == 0 || string.Join(" ", model.Name, model.Id, model.Family, model.Category, model.Quantization)
                .Contains(query, StringComparison.CurrentCultureIgnoreCase))
            .Select(model => _rows[model.Id]).ToArray();
        // Change only membership/order; stable row instances retain keyboard focus and virtualization state.
        // 仅调整成员及顺序，稳定行实例保留键盘焦点和虚拟化状态，不清空重建列表。
        for (int index = 0; index < desired.Length; index++)
        {
            if (index < _visibleRows.Count && ReferenceEquals(_visibleRows[index], desired[index])) continue;
            int previous = _visibleRows.IndexOf(desired[index]);
            if (previous >= 0) _visibleRows.Move(previous, index);
            else _visibleRows.Insert(index, desired[index]);
        }
        while (_visibleRows.Count > desired.Length) _visibleRows.RemoveAt(_visibleRows.Count - 1);
        _empty.Visibility = desired.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
        _count.Text = string.Format(UiText.Get("共 {0} 个模型 · 已安装 {1} 个"), _snapshot?.Models.Length ?? 0,
            _snapshot?.Models.Count(model => model.Installed) ?? 0);
    }

    private void RenderStorageSpace()
    {
        long installedBytes = _snapshot?.Models.Where(model => model.Installed)
            .Sum(model => Math.Max(model.DownloadBytes, model.DownloadedBytes)) ?? 0;
        string installed = FormatBytes(installedBytes);
        _storageSpace.Text = _storageAvailableBytes is { } available
            ? string.Format(UiText.Get("磁盘可用 {0} · 已安装模型约 {1}"), FormatBytes(available), installed)
            : string.Format(UiText.Get("已安装模型约 {0}"), installed);
    }

    private async Task RefreshStorageSpaceAsync(string directory)
    {
        bool changed = !string.Equals(_storageDirectory, directory, StringComparison.OrdinalIgnoreCase);
        if (!changed && DateTimeOffset.UtcNow - _storageRefreshAt < TimeSpan.FromSeconds(15)) return;
        _storageDirectory = directory;
        _storageRefreshAt = DateTimeOffset.UtcNow;
        if (changed) _storageAvailableBytes = null;
        // Read one drive value off the UI thread; model sizes come from the existing snapshot, never a directory scan.
        // 仅在后台读取磁盘可用量；模型占用复用已有快照，不递归扫描目录。
        long? available = await Task.Run(() =>
        {
            try { return (long?)new DriveInfo(Path.GetPathRoot(directory) ?? directory).AvailableFreeSpace; }
            catch (IOException) { return null; }
            catch (UnauthorizedAccessException) { return null; }
            catch (ArgumentException) { return null; }
        });
        if (_closed || !string.Equals(_snapshot?.InstallDirectory, directory, StringComparison.OrdinalIgnoreCase)) return;
        _storageAvailableBytes = available;
        RenderStorageSpace();
    }

    private async Task RunOperationAsync(Func<Task<LocalModelsSnapshot>> operation, string? modelId = null, bool settings = false)
    {
        if (_closed || settings && _settingsBusy || modelId is not null && !_busyModels.Add(modelId)) return;
        long epoch = ++_snapshotEpoch;
        if (settings) _settingsBusy = true;
        if (_snapshot is { } before) ApplySnapshot(before);
        _notice.IsOpen = false;
        try
        {
            var snapshot = await operation();
            if (epoch == _snapshotEpoch) ApplySnapshot(snapshot);
            else await RefreshAsync();
        }
        catch (OperationCanceledException) when (_closed) { }
        catch (Exception error) { ShowError(error); }
        finally
        {
            if (modelId is not null) _busyModels.Remove(modelId);
            if (settings) _settingsBusy = false;
            if (!_closed && _snapshot is { } current) ApplySnapshot(current);
        }
    }

    private void ShowError(Exception error)
    {
        if (_closed) return;
        _notice.Severity = InfoBarSeverity.Error;
        _notice.Message = error is OperationCanceledException ? UiText.Get("本地模型请求超时，请刷新查看状态。") : error.Message;
        _notice.IsOpen = true;
    }

    private void LanguageChanged(object? sender, EventArgs args)
    {
        if (_closed) return;
        if (_snapshot is { } snapshot) ApplySnapshot(snapshot);
    }
}
