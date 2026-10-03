using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml.Controls;

namespace MemoryUiSmoke;

public partial class App
{
    private async Task CheckEditingAndDialogsAsync()
    {
        await OpenEntryAsync(0);
        Guid entryId = _viewModel.SelectedEntry!.Id;
        string original = ContentBox.Text;
        Check(Label("MemorySourceLabel").Text.Contains("手动添加") && Label("MemorySourceStatusLabel").Text.Contains("正常使用"),
            "native editor shows manual source and active backend status");
        await CaptureAsync("02-editor-zh");
        const string edited = "EDIT_SYNTHETIC 中文草稿\r第二行保持原样。";
        NativeUi.SetText(ContentBox, edited);
        await WaitAsync(() => _viewModel.EditorContent == edited, "native content input reaches the production editor state");
        NativeUi.ById<ComboBox>(_root, "MemoryKindPicker").SelectedIndex = 2;
        await WaitAsync(() => _viewModel.EditorContent == edited && _viewModel.EditorKind == MemoryKinds.Decision && Button("SaveMemoryButton").IsEnabled,
            "native editor input and kind selection enable Save");
        Check(_api.Snapshot(_user).Entries[0].Content == original && _api.Writes == 0,
            "editing leaves authoritative fixture data unchanged before Save");
        NativeUi.SetText(SearchBox, "手动偏好");
        await WaitAsync(() => EntryList.Items.Count == 1, "native search filters the visible memory list");
        UiText.Initialize("en");
        await WaitAsync(() => Button("SaveMemoryButton").Content?.ToString() == "Save" && _window!.Title == UiText.Get("KYNXA · 记忆管理"),
            "live language change updates the open native memory window");
        Check(ContentBox.Text == edited && SearchBox.Text == "手动偏好" && _viewModel.SelectedEntry?.Id == entryId &&
            _viewModel.EditorKind == MemoryKinds.Decision && EntryList.Items.Count == 1,
            "live language change preserves editor text, kind, search, and selected entry identity");
        Check(ContentBox.PlaceholderText == UiText.Get("输入需要保留的记忆") && Label("MemorySourceLabel").Text.Contains("Manually added"),
            "live language change updates native placeholders and source metadata");
        Check(NativeUi.Descendants<TextBlock>(NativeUi.ById<ComboBox>(_root, "MemoryKindPicker"))
                .Any(label => NativeUi.IsVisible(label) && label.Text == UiText.Get("决定")),
            "live language change updates the visible selected kind caption in the native ComboBox");
        await CaptureAsync("03-editor-en");
        NativeUi.SetText(SearchBox, string.Empty);
        await SettleAsync();
        await SaveAsync();
        Check(_api.Snapshot(_user).Entries.Single(entry => entry.Id == entryId).Content == edited && !_viewModel.HasChanges,
            "explicit native Save persists edited content and clears only the committed draft");
        UiText.Initialize("zh-CN");
        await SettleAsync();

        NativeUi.Invoke(Button("NewMemoryButton"));
        await WaitAsync(() => _viewModel.IsNew && ContentBox.Text.Length == 0, "native New opens an empty unsaved editor");
        const string created = "CREATE_SYNTHETIC 新增手动记忆";
        NativeUi.SetText(ContentBox, created);
        await SettleAsync();
        int beforeCreate = _api.Writes;
        Check(_api.Snapshot(_user).Entries.Length == 4 && _api.Writes == beforeCreate,
            "typing a new memory does not write before explicit Save");
        await SaveAsync();
        Check(_api.Writes == beforeCreate + 1 && EntryList.Items.Count == 5 && _viewModel.SelectedEntry?.Source.Type == "manual",
            "explicit native Save creates one manual confirmed memory and selects it");
        Guid createdId = _viewModel.SelectedEntry!.Id;
        int beforeDelete = _api.Writes;
        NativeUi.Invoke(Button("DeleteMemoryButton"));
        var dialog = await WaitForDialogAsync("native Delete opens a confirmation before writing");
        Check(_api.Writes == beforeDelete && _api.Snapshot(_user).Entries.Any(entry => entry.Id == createdId),
            "opening delete confirmation does not mutate memory data");
        UiText.Initialize("en");
        await WaitAsync(() => dialog.Title?.ToString() == UiText.Get("删除这条记忆？") && dialog.PrimaryButtonText == "Delete" && dialog.CloseButtonText == "Cancel",
            "language change updates an already open native confirmation dialog");
        await CaptureAsync("04-delete-confirm-en");
        await DismissDialogAsync(dialog, primary: false);
        Check(_api.Writes == beforeDelete && _viewModel.SelectedEntry?.Id == createdId && ContentBox.Text == created,
            "native cancel preserves the selected memory and editor content");
        NativeUi.Invoke(Button("DeleteMemoryButton"));
        dialog = await WaitForDialogAsync("native Delete can reopen its confirmation");
        await DismissDialogAsync(dialog, primary: true);
        await WaitAsync(() => _viewModel.Status == MemoryManagementStatus.Deleted && !_viewModel.IsBusy,
            "native confirmation deletes the intended entry and reloads its backend projection");
        Check(_api.Writes == beforeDelete + 1 && !_api.Snapshot(_user).Entries.Any(entry => entry.Id == createdId) &&
            EntryList.Items.Count == 4 && _viewModel.IsNew,
            "confirmed deletion removes only the selected entry and resets its editor");
        UiText.Initialize("zh-CN");
        await SettleAsync();

        await OpenEntryAsync(1);
        Check(Label("MemorySourceLabel").Text.Contains("用户消息") && Label("MemorySourceStatusLabel").Text.Contains("来源已归档"),
            "archived user-message source remains visible with its backend archived status");
        await OpenEntryAsync(2);
        Check(Label("MemorySourceStatusLabel").Text.Contains("暂停使用") && Label("MemorySourceStatusLabel").Text.Contains("来源不可用"),
            "unavailable source remains visible and distinguishes suspended use from deletion");
        await CaptureAsync("05-unavailable-source");

        const string draft = "UNSAVED_SYNTHETIC 保留输入";
        Guid selected = _viewModel.SelectedEntry!.Id;
        NativeUi.SetText(ContentBox, draft);
        await SettleAsync();
        NativeUi.Invoke(Button("NewMemoryButton"));
        dialog = await WaitForDialogAsync("native New confirms discarding a pending editor draft");
        await DismissDialogAsync(dialog, primary: false);
        Check(ContentBox.Text == draft && _viewModel.SelectedEntry?.Id == selected,
            "cancelling New retains the draft and selected entry");
        await NativeUi.InvokeListItemAsync(EntryList, EntryList.Items[0]);
        dialog = await WaitForDialogAsync("native entry switch confirms discarding pending edits");
        await DismissDialogAsync(dialog, primary: false);
        Check(ContentBox.Text == draft && _viewModel.SelectedEntry?.Id == selected,
            "cancelling entry switch retains the draft and selected entry");
        NativeUi.Invoke(Button("MemoryScopeProjectButton"));
        dialog = await WaitForDialogAsync("native scope switch confirms discarding pending edits");
        await DismissDialogAsync(dialog, primary: false);
        Check(_viewModel.Target?.Scope == MemoryScopes.User && ContentBox.Text == draft,
            "cancelling scope switch leaves the active scope and draft intact");
        NativeUi.RequestClose(_window!);
        dialog = await WaitForDialogAsync("native window close confirms discarding pending edits");
        await DismissDialogAsync(dialog, primary: false);
        Check(!_closed && !_api.IsDisposed && ContentBox.Text == draft,
            "cancelling native window close retains the active editor and API lifetime");
        NativeUi.Invoke(Button("NewMemoryButton"));
        dialog = await WaitForDialogAsync("native New reopens the pending-draft confirmation");
        await DismissDialogAsync(dialog, primary: true);
        await WaitAsync(() => _viewModel.IsNew && ContentBox.Text.Length == 0 && !_viewModel.HasChanges,
            "confirming discard opens a clean new editor without writing the discarded draft");
    }
}
