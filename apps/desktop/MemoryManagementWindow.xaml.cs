using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Globalization;
using System.Net;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics;

namespace KYNXA_Desktop;

/// <summary>
/// Presentation and confirmations only. The gateway owns memory persistence.
/// 本窗口仅负责展示和确认；记忆的正式持久化由网关负责。
/// </summary>
public sealed partial class MemoryManagementWindow : Window
{
    private readonly MemoryManagementViewModel _viewModel;
    private readonly IReadOnlyList<MemoryTarget> _targets;
    private readonly MemoryTarget? _initialTarget;
    private readonly ObservableCollection<MemoryDisplayRow> _rows = [];
    private string _scope = MemoryScopes.User;
    private bool _updating;
    private bool _closed;
    private bool _loaded;
    private bool _queued;
    private bool _rowsChanged = true;
    private bool _refreshKindDisplay;
    private bool _dialogOpen;
    private bool _allowClose;

    public MemoryManagementWindow(IReadOnlyList<MemoryTarget> targets, MemoryTarget? initialTarget = null,
        MemoryManagementViewModel? viewModel = null)
    {
        InitializeComponent();
        AppearanceService.TrackWindow(this);
        _targets = targets.Where(target => target.Scope != MemoryScopes.User).DistinctBy(target => (target.Scope, target.Id)).ToArray();
        _initialTarget = initialTarget;
        _viewModel = viewModel ?? new MemoryManagementViewModel();
        MemoryEntryList.ItemsSource = _rows;
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(MemoryTitleBar);
        AppWindow.Resize(new SizeInt32(960, 680));
        _viewModel.PropertyChanged += ViewModel_Changed;
        _viewModel.VisibleEntries.CollectionChanged += Entries_Changed;
        UiText.LanguageChanged += Language_Changed;
        AppWindow.Closing += Window_Closing;
        Closed += Window_Closed;
        Render();
    }

    public bool HasPendingChanges => _viewModel.IsBusy || _viewModel.HasChanges;

    // Called only when the application exits or after the migration guard checked the editor.
    // 仅在应用退出，或迁移保护已检查编辑器之后调用。
    public void CloseForOwner()
    {
        _allowClose = true;
        Close();
    }

    private async void MemoryRoot_Loaded(object sender, RoutedEventArgs e)
    {
        if (_loaded) return;
        _loaded = true;
        string scope = _initialTarget?.Scope ?? MemoryScopes.User;
        await ChangeScopeAsync(scope, _initialTarget);
    }

    private void MemoryRoot_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        bool narrow = e.NewSize.Width < 720;
        MemoryListColumn.Width = narrow ? new GridLength(1, GridUnitType.Star) : new GridLength(320);
        MemoryEditorColumn.Width = narrow ? new GridLength(0) : new GridLength(1, GridUnitType.Star);
        Grid.SetColumnSpan(MemoryListPanel, narrow ? 2 : 1);
        Grid.SetRowSpan(MemoryListPanel, narrow ? 1 : 2);
        Grid.SetColumn(MemoryEditorPanel, narrow ? 0 : 1);
        Grid.SetRow(MemoryEditorPanel, narrow ? 1 : 0);
        Grid.SetColumnSpan(MemoryEditorPanel, narrow ? 2 : 1);
        Grid.SetRowSpan(MemoryEditorPanel, narrow ? 1 : 2);
    }

    private void ViewModel_Changed(object? sender, PropertyChangedEventArgs e) => QueueRender();
    private void Entries_Changed(object? sender, System.Collections.Specialized.NotifyCollectionChangedEventArgs e)
    {
        _rowsChanged = true;
        QueueRender();
    }

    private void Language_Changed(object? sender, EventArgs e)
    {
        _rowsChanged = true;
        _refreshKindDisplay = true;
        QueueRender();
    }

    private void QueueRender()
    {
        if (_closed || _queued) return;
        _queued = true;
        DispatcherQueue.TryEnqueue(() =>
        {
            _queued = false;
            if (!_closed) Render();
        });
    }

    private void Render()
    {
        _updating = true;
        try
        {
            Title = UiText.Get("KYNXA · 记忆管理");
            foreach (var button in new[] { MemoryScopeChatButton, MemoryScopeProjectButton, MemoryScopeUserButton })
                button.Background = (Brush)Application.Current.Resources[(string)button.Tag == _scope ? "KynxaSelectionBrush" : "KynxaMainBrush"];
            MemoryContextPicker.Visibility = _scope == MemoryScopes.User ? Visibility.Collapsed : Visibility.Visible;
            MemoryEditorHeading.Text = UiText.Get(_viewModel.IsNew ? "新增记忆" : "编辑记忆");
            MemoryScopeLabel.Text = _viewModel.Target?.DisplayName ?? UiText.Get(_scope == MemoryScopes.Chat ? "暂无已保存的聊天" : "暂无工作");
            if (_scope == MemoryScopes.User) MemoryScopeLabel.Text = UiText.Get("全局记忆");
            if (IsArchivedProject) MemoryScopeLabel.Text += "\n" + UiText.Get("工作已归档，共享记忆暂停用于聊天。");
            if (MemoryContentBox.Text != _viewModel.EditorContent) MemoryContentBox.Text = _viewModel.EditorContent;
            var selectedKind = MemoryKindPicker.Items.OfType<ComboBoxItem>().FirstOrDefault(item => (string)item.Tag == _viewModel.EditorKind);
            // ComboBox caches the selection box text; refresh that presentation after localization.
            // The guard prevents these selection events from changing the editor's stable kind ID.
            // 本地化后刷新 ComboBox 缓存的选中文案；保护标记避免刷新事件修改稳定的记忆类型 ID。
            if (_refreshKindDisplay) MemoryKindPicker.SelectedItem = null;
            MemoryKindPicker.SelectedItem = selectedKind;
            _refreshKindDisplay = false;
            bool editable = _viewModel.Target is not null && !_viewModel.IsBusy;
            MemoryContentBox.IsEnabled = editable;
            MemoryContentBox.IsReadOnly = false;
            MemoryKindPicker.IsEnabled = editable;
            NewMemoryButton.IsEnabled = editable && _viewModel.IsLoaded;
            RefreshMemoryButton.IsEnabled = _viewModel.Target is not null && !_viewModel.IsBusy;
            SaveMemoryButton.IsEnabled = _viewModel.CanSave;
            SaveMemoryButton.Content = UiText.Get(_viewModel.IsDraft ? "保存草稿" : "保存");
            ConfirmMemoryButton.IsEnabled = _viewModel.CanConfirm;
            ConfirmMemoryButton.Visibility = _viewModel.IsDraft ? Visibility.Visible : Visibility.Collapsed;
            ConfirmMemoryButton.Content = UiText.Get(_viewModel.SelectedEntry?.Proposal?.Action switch
            {
                "update" => "确认更新", "delete" => "确认删除", _ => "确认记忆"
            });
            DeleteMemoryButton.IsEnabled = _viewModel.CanDelete;
            DeleteMemoryButton.Content = UiText.Get(_viewModel.IsDraft ? "丢弃" : "删除");
            DeleteMemoryButton.Visibility = _viewModel.IsNew ? Visibility.Collapsed : Visibility.Visible;
            MemoryEntryList.IsEnabled = !_viewModel.IsBusy;
            MemoryInputHint.Text = _viewModel.EditorContent.Length == 0 ? string.Empty : _viewModel.InputError switch
            {
                MemoryInputError.EmptyContent => UiText.Get("内容不能为空。"),
                MemoryInputError.ContentTooLong => string.Format(UiText.Get("记忆内容不能超过 {0} 个字符。"), MemoryInputValidation.MaximumContentLength),
                MemoryInputError.ContentContainsNull => UiText.Get("内容包含无效字符。"),
                MemoryInputError.InvalidKind => UiText.Get("请选择有效的记忆类型。"),
                _ => string.Format(UiText.Get("{0} / {1} 字符"), _viewModel.EditorContent.Length, MemoryInputValidation.MaximumContentLength)
            };
            RenderSource();
            if (_rowsChanged) RebuildRows();
            foreach (var row in _rows) row.SetSelected(row.Entry.Id == _viewModel.SelectedEntry?.Id);
            MemoryCountLabel.Text = string.Format(UiText.Get("{0} 条记忆"), _rows.Count);
            MemoryEmptyPanel.Visibility = _rows.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
            MemoryLoadingRing.IsActive = _viewModel.IsBusy;
            MemoryLoadingRing.Visibility = _viewModel.IsBusy ? Visibility.Visible : Visibility.Collapsed;
            MemoryEmptyLabel.Text = EmptyStateText();
            RenderStatus();
        }
        finally { _updating = false; }
    }

    private void RebuildRows()
    {
        _rowsChanged = false;
        string search = MemorySearchBox.Text.Trim();
        _rows.Clear();
        foreach (var entry in _viewModel.VisibleEntries)
            if (search.Length == 0 || entry.Content.Contains(search, StringComparison.OrdinalIgnoreCase))
                _rows.Add(new MemoryDisplayRow(entry, $"{KindLabel(entry.Kind)} · {UiText.Get(entry.Status == MemoryStatuses.Draft ? "待确认" : "已确认")} · {SourceStatus(entry)}"));
    }

    private string EmptyStateText()
    {
        if (_viewModel.IsBusy) return UiText.Get("正在读取记忆…");
        if (_viewModel.Target is null)
            return UiText.Get(_scope == MemoryScopes.Chat ? "发送消息后，即可管理这个聊天的记忆。" : "创建工作后，即可管理工作记忆。");
        if (!_viewModel.IsLoaded) return UiText.Get("未能读取记忆，请刷新重试。");
        return UiText.Get(MemorySearchBox.Text.Length > 0 ? "没有匹配的记忆" : "暂无记忆，点击新增记忆。");
    }

    private static string KindLabel(string kind) => UiText.Get(kind switch
    {
        MemoryKinds.Preference => "偏好", MemoryKinds.Decision => "决定", _ => "事实"
    });

    private bool IsArchivedProject => _viewModel.Target is { Scope: MemoryScopes.Project, IsArchived: true };

    private string SourceStatus(MemoryEntry entry)
    {
        if (entry.Status == MemoryStatuses.Draft) return UiText.Get("草稿不用于模型上下文。");
        if (entry.Active == false) return UiText.Get("暂停使用");
        if (IsArchivedProject) return UiText.Get("工作已归档，暂停共享");
        if (entry.SourceAvailable == false) return UiText.Get("来源不可用");
        return UiText.Get(entry.SourceArchived == true ? "来源已归档" : "正常使用");
    }

    private void RenderSource()
    {
        var entry = _viewModel.SelectedEntry;
        bool manual = entry is null || entry.Source.Type == "manual";
        string source = UiText.Get(manual ? "手动添加" : "用户消息");
        if (!manual && entry!.Source.ConversationId is { } id)
            source += " · " + (_targets.FirstOrDefault(target => target.Scope == MemoryScopes.Chat && target.Id == id)?.DisplayName ?? UiText.Get("来源聊天"));
        MemorySourceLabel.Text = UiText.Get("来源") + " · " + source;
        MemoryEntryStatusLabel.Text = UiText.Get(entry is null ? "手动保存即确认此范围的记忆。" :
            entry.Status == MemoryStatuses.Draft ? "待确认" : "已确认");
        MemorySourceStatusLabel.Text = entry is null ? UiText.Get(IsArchivedProject ? "工作已归档，共享记忆暂停用于聊天。" : "保存后用于当前范围的上下文。") : SourceStatus(entry)
            + (entry.Active == false && entry.SourceAvailable == false ? " · " + UiText.Get("来源不可用") : string.Empty)
            + (entry.Active == false && entry.SourceArchived == true ? " · " + UiText.Get("来源已归档") : string.Empty);
        if (entry?.Status == MemoryStatuses.Draft && _viewModel.HasChanges)
            MemorySourceStatusLabel.Text += "\n" + UiText.Get("请先保存草稿修改，再确认。");
        if (entry?.Status == MemoryStatuses.Draft && entry.Proposal?.Target is { } proposedTarget &&
            !_viewModel.Entries.Any(item => item.Id == proposedTarget.Id && item.Status == MemoryStatuses.Confirmed &&
                item.Revision == proposedTarget.EntryRevision))
            MemorySourceStatusLabel.Text += "\n" + UiText.Get("目标已变化，请重新读取。");
        MemoryRevisionLabel.Text = _viewModel.ScopeRevision is { } revision
            ? string.Format(UiText.Get("范围版本 {0} · 条目版本 {1}"), revision, entry?.Revision ?? 0) : string.Empty;
        var proposal = entry?.Proposal;
        MemoryProposalLabel.Visibility = proposal is null ? Visibility.Collapsed : Visibility.Visible;
        MemoryProposalLabel.Text = proposal is null ? string.Empty : string.Join("\n", new[]
        {
            UiText.Get("建议动作") + " · " + UiText.Get(proposal.Action switch { "update" => "更新已有记忆", "delete" => "删除已有记忆", _ => "新增记忆" }),
            UiText.Get(proposal.IsInference ? "推断建议，请核对用户原话。" : "请核对建议与用户原话。"),
            UiText.Get("理由") + " · " + proposal.Reason,
            proposal.Target is { } target ? UiText.Get("目标记忆") + " · " +
                (_viewModel.Entries.FirstOrDefault(item => item.Id == target.Id)?.Content ?? UiText.Get("目标已变化，请重新读取。")) : string.Empty,
            proposal.Conflicts.Length > 0 ? UiText.Get("可能冲突") + " · " + string.Join("; ", proposal.Conflicts) : string.Empty
        }.Where(line => line.Length > 0));
        var quotes = proposal?.Quotes ?? entry?.Candidate?.Quotes;
        MemoryQuotesLabel.Visibility = quotes is { Length: > 0 } ? Visibility.Visible : Visibility.Collapsed;
        MemoryQuotesLabel.Text = quotes is null ? string.Empty : UiText.Get("用户原话") + "\n" +
            string.Join("\n\n", quotes.Select(quote => $"{quote.Text}\n{UiText.Get("来源消息")} · {quote.MessageId:D}"));
        MemoryUpdatedLabel.Visibility = entry is null ? Visibility.Collapsed : Visibility.Visible;
        MemoryUpdatedLabel.Text = entry is null ? string.Empty : UiText.Get("更新时间") + " · "
            + entry.UpdatedAt.ToLocalTime().ToString("g", UiText.Language == "en" ? CultureInfo.GetCultureInfo("en-US") : CultureInfo.GetCultureInfo("zh-CN"));
    }

    private void RenderStatus()
    {
        string? key = _viewModel.Status switch
        {
            MemoryManagementStatus.Conflict => "记忆已在其他位置更新。已刷新列表并保留你的编辑，请核对后再次保存。",
            MemoryManagementStatus.ConflictRefreshFailed => "记忆版本冲突，刷新失败。编辑内容已保留，请先刷新。",
            MemoryManagementStatus.SavedRefreshFailed => "已保存，但列表刷新失败。请刷新，避免重复添加。",
            MemoryManagementStatus.DeletedRefreshFailed => "已删除，但列表刷新失败。请刷新。",
            MemoryManagementStatus.ConfirmedRefreshFailed => "已确认，但列表刷新失败。请刷新核对当前状态。",
            MemoryManagementStatus.WriteOutcomeUnknown => "暂时无法确认是否保存成功。请刷新检查列表，避免重复添加。",
            MemoryManagementStatus.EntryMissing => "这条记忆已被删除。编辑内容已保留，可复制后新增。",
            MemoryManagementStatus.InvalidContent => "请检查记忆内容。",
            MemoryManagementStatus.InvalidKind => "请选择有效的记忆类型。",
            MemoryManagementStatus.Error => "记忆操作失败，请重试。",
            _ => null
        };
        MemoryStatusBar.IsOpen = key is not null;
        MemoryStatusBar.Severity = _viewModel.Status == MemoryManagementStatus.Error ? InfoBarSeverity.Error : InfoBarSeverity.Warning;
        if (key is not null)
        {
            MemoryStatusBar.Message = UiText.Get(key);
            if (_viewModel.Status == MemoryManagementStatus.Error && _viewModel.Error is GatewayApiException error)
                MemoryStatusBar.Message = error.StatusCode == HttpStatusCode.NotFound && string.IsNullOrEmpty(error.ErrorCode)
                    ? UiText.Get("当前网关不支持记忆管理，请结束旧网关后重新打开 KYNXA。")
                    : MemoryStatusBar.Message + " " + error.Message;
        }
    }

    private async Task ChangeScopeAsync(string scope, MemoryTarget? preferred = null)
    {
        _scope = scope;
        var targets = _targets.Where(target => target.Scope == scope).ToArray();
        var target = scope == MemoryScopes.User ? new MemoryTarget(scope, null, UiText.Get("全局记忆"))
            : targets.FirstOrDefault(item => item.Id == preferred?.Id) ?? targets.FirstOrDefault();
        _updating = true;
        MemoryContextPicker.ItemsSource = targets;
        MemoryContextPicker.SelectedItem = target;
        _updating = false;
        await _viewModel.SelectTargetAsync(target);
    }

    private async void Scope_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: string scope } || scope == _scope || !await ConfirmDiscardAsync()) return;
        if (!_closed) await ChangeScopeAsync(scope);
    }

    private async void Context_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_updating || _closed || MemoryContextPicker.SelectedItem is not MemoryTarget target || target == _viewModel.Target) return;
        if (!await ConfirmDiscardAsync())
        {
            _updating = true;
            MemoryContextPicker.SelectedItem = _viewModel.Target;
            _updating = false;
            return;
        }
        if (!_closed) await _viewModel.SelectTargetAsync(target);
    }

    private async void Entry_Click(object sender, ItemClickEventArgs e)
    {
        if (e.ClickedItem is not MemoryDisplayRow row || row.Entry.Id == _viewModel.SelectedEntry?.Id || !await ConfirmDiscardAsync()) return;
        if (!_closed) _viewModel.BeginEdit(row.Entry);
    }

    private async void New_Click(object sender, RoutedEventArgs e)
    {
        if (await ConfirmDiscardAsync() && !_closed) _viewModel.BeginNew();
    }

    private async void Refresh_Click(object sender, RoutedEventArgs e) => await _viewModel.RefreshAsync();
    private async void Save_Click(object sender, RoutedEventArgs e) => await _viewModel.SaveAsync();
    private async void Confirm_Click(object sender, RoutedEventArgs e)
    {
        if (!_viewModel.CanConfirm || _dialogOpen) return;
        var reviewedEntry = _viewModel.SelectedEntry!;
        long? reviewedRevision = _viewModel.ScopeRevision;
        string? action = reviewedEntry.Proposal?.Action;
        if (action is "update" or "delete" && !await ConfirmAsync(
            action == "update" ? "确认更新已有记忆？" : "确认删除已有记忆？",
            action == "update" ? "此建议将替换上方显示的目标记忆，请核对内容与来源。" : "此建议将删除上方显示的目标记忆，请核对目标与来源。",
            action == "update" ? "确认更新" : "确认删除")) return;
        // An open dialog grants authority only to the entry the user reviewed.
        // 已打开的确认框仅授权用户刚才核对的条目，切换或关闭后不继续执行。
        if (!_closed && ReferenceEquals(_viewModel.SelectedEntry, reviewedEntry) && _viewModel.ScopeRevision == reviewedRevision)
            await _viewModel.ConfirmAsync();
    }
    private async void Delete_Click(object sender, RoutedEventArgs e)
    {
        if (!_viewModel.CanDelete || _dialogOpen) return;
        if (await ConfirmAsync(_viewModel.IsDraft ? "丢弃这条草稿？" : "删除这条记忆？",
            _viewModel.IsDraft ? "丢弃后，此建议不会进入模型上下文。" : "删除后，它将不再用于模型上下文。",
            _viewModel.IsDraft ? "丢弃" : "删除") && !_closed)
            await _viewModel.DeleteAsync();
    }

    private void Content_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (!_updating && _viewModel is not null) _viewModel.EditorContent = MemoryContentBox.Text;
    }

    private void Kind_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (!_updating && _viewModel is not null && MemoryKindPicker.SelectedItem is ComboBoxItem { Tag: string kind }) _viewModel.EditorKind = kind;
    }

    private void Search_TextChanged(object sender, TextChangedEventArgs e)
    {
        _rowsChanged = true;
        if (_viewModel is not null) QueueRender();
    }

    private Task<bool> ConfirmDiscardAsync() => !_viewModel.HasChanges ? Task.FromResult(!_dialogOpen) :
        ConfirmAsync("放弃未保存的修改？", "当前编辑尚未保存。", "放弃修改");

    private async Task<bool> ConfirmAsync(string title, string content, string action)
    {
        if (_closed || _dialogOpen || MemoryRoot.XamlRoot is null) return false;
        _dialogOpen = true;
        var dialog = new ContentDialog { XamlRoot = MemoryRoot.XamlRoot, DefaultButton = ContentDialogButton.None };
        dialog.PrimaryButtonStyle = (Style)Application.Current.Resources["KynxaQuietButtonStyle"];
        dialog.CloseButtonStyle = (Style)Application.Current.Resources["KynxaQuietButtonStyle"];
        dialog.Resources["SystemControlFocusVisualPrimaryBrush"] = Application.Current.Resources["KynxaSecondaryTextBrush"];
        UiLocalization.Bind(dialog, ContentDialog.TitleProperty, title);
        UiLocalization.Bind(dialog, ContentDialog.ContentProperty, content);
        UiLocalization.Bind(dialog, ContentDialog.PrimaryButtonTextProperty, action);
        UiLocalization.Bind(dialog, ContentDialog.CloseButtonTextProperty, "取消");
        try { return await DialogPresentation.ShowAsync(dialog) == ContentDialogResult.Primary; }
        finally { _dialogOpen = false; }
    }

    private async void Window_Closing(AppWindow sender, AppWindowClosingEventArgs e)
    {
        if (_allowClose || !_viewModel.HasChanges) return;
        e.Cancel = true;
        if (await ConfirmDiscardAsync() && !_closed)
        {
            _allowClose = true;
            Close();
        }
    }

    private void Window_Closed(object sender, WindowEventArgs e)
    {
        _closed = true;
        UiText.LanguageChanged -= Language_Changed;
        _viewModel.PropertyChanged -= ViewModel_Changed;
        _viewModel.VisibleEntries.CollectionChanged -= Entries_Changed;
        AppWindow.Closing -= Window_Closing;
        _viewModel.Dispose();
    }
}

internal sealed class MemoryDisplayRow(MemoryEntry entry, string detail) : INotifyPropertyChanged
{
    private bool _selected;
    public MemoryEntry Entry { get; } = entry;
    public string Detail { get; } = detail;
    public Brush Background => (Brush)Application.Current.Resources[_selected ? "KynxaSelectionBrush" : "KynxaMainBrush"];
    public event PropertyChangedEventHandler? PropertyChanged;
    public void SetSelected(bool selected)
    {
        if (_selected == selected) return;
        _selected = selected;
        PropertyChanged?.Invoke(this, new(nameof(Background)));
    }
}
