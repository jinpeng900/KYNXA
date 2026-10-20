using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml.Controls;

namespace MemoryUiSmoke;

public partial class App
{
    private async Task CheckDraftConfirmationAsync()
    {
        var draft = PendingEntry("PENDING_SYNTHETIC 待确认记忆");
        _api.Seed(_chat, draft);
        NativeUi.Invoke(Button("MemoryScopeChatButton"));
        await WaitAsync(() => _viewModel.Target == _chat && !_viewModel.IsBusy, "native chat scope loads a saved pending candidate");
        await OpenEntryAsync(0);
        Check(Label("MemoryEntryStatusLabel").Text == UiText.Get("待确认") &&
            Label("MemorySourceStatusLabel").Text.Contains(UiText.Get("草稿不用于模型上下文。")) &&
            Label("MemoryQuotesLabel").Text.Contains("SAVED_USER_QUOTE") && Label("MemoryRevisionLabel").Text.Contains("1"),
            "native pending editor exposes confirmation status, original user quotation, and scope revision");
        Check(Button("ConfirmMemoryButton").IsEnabled && Button("SaveMemoryButton").Content?.ToString() == UiText.Get("保存草稿"),
            "pending memory exposes separate Save draft and explicit Confirm actions");
        int before = _api.Writes;
        NativeUi.SetText(ContentBox, "EDITED_PENDING_SYNTHETIC 保存草稿不代表确认");
        await WaitAsync(() => Button("SaveMemoryButton").IsEnabled && !Button("ConfirmMemoryButton").IsEnabled,
            "unsaved pending edits require Save before confirmation");
        await SaveAsync();
        Check(_api.Snapshot(_chat).Entries[0].Status == MemoryStatuses.Draft && _api.Snapshot(_chat).Entries[0].Active == false,
            "native Save draft preserves pending status and excludes it from model context");
        NativeUi.Invoke(Button("ConfirmMemoryButton"));
        await WaitAsync(() => !_viewModel.IsBusy && _viewModel.Status == MemoryManagementStatus.Confirmed,
            "native explicit Confirm commits the reviewed candidate");
        Check(_api.Writes == before + 2 && _api.Snapshot(_chat).Entries[0].Status == MemoryStatuses.Confirmed &&
            Label("MemoryEntryStatusLabel").Text == UiText.Get("已确认") &&
            Button("ConfirmMemoryButton").Visibility == Microsoft.UI.Xaml.Visibility.Collapsed,
            "confirmation reloads the source state and removes the completed Confirm action");

        foreach (string action in new[] { "update", "delete" })
        {
            var target = FakeMemoryApi.Entry(_chat, "EXISTING_TARGET_SYNTHETIC 待核对的目标记忆");
            var suggestion = PendingEntry("PROPOSAL_SYNTHETIC " + action);
            suggestion = suggestion with
            {
                Candidate = null,
                Proposal = new("model-memory-proposal-v1", action, suggestion.Candidate!.Quotes,
                    "SYNTHETIC_REASON 用户原话需核对", true, ["SYNTHETIC_CONFLICT"], new string('b', 64),
                    new(target.Id, _chat.Scope, _chat.ScopeId, 1, target.Revision, new string('c', 64)))
            };
            _api.Seed(_chat, target, suggestion);
            NativeUi.Invoke(Button("RefreshMemoryButton"));
            await WaitAsync(() => !_viewModel.IsBusy && _viewModel.Entries.Count == 2, "native Refresh loads a mutation proposal with its existing target");
            await OpenEntryAsync(1);
            Check(Label("MemoryProposalLabel").Text.Contains("EXISTING_TARGET_SYNTHETIC") &&
                Label("MemoryProposalLabel").Text.Contains("SYNTHETIC_REASON") &&
                Label("MemoryProposalLabel").Text.Contains("SYNTHETIC_CONFLICT"),
                "native proposal displays its target, reason, inference status, and conflicts before mutation");
            before = _api.Writes;
            NativeUi.Invoke(Button("ConfirmMemoryButton"));
            var dialog = await WaitForDialogAsync("native " + action + " proposal opens an explicit mutation confirmation");
            Check(_api.Writes == before, "opening a proposal confirmation grants no write before acceptance");
            await DismissDialogAsync(dialog, primary: false);
            Check(_api.Writes == before && _viewModel.SelectedEntry?.Id == suggestion.Id,
                "cancelling the proposal retains both target and pending suggestion");
            NativeUi.Invoke(Button("ConfirmMemoryButton"));
            dialog = await WaitForDialogAsync("native reviewed proposal confirmation can reopen");
            await DismissDialogAsync(dialog, primary: true);
            await WaitAsync(() => !_viewModel.IsBusy && _viewModel.Status == MemoryManagementStatus.Confirmed,
                "native accepted " + action + " applies the exact reviewed mutation");
            Check(_api.Writes == before + 1 && (action == "update"
                ? _viewModel.SelectedEntry?.Id == target.Id && ContentBox.Text == suggestion.Content
                : _viewModel.IsNew && _viewModel.Entries.Count == 0),
                "accepted proposal reloads the resulting target or removes the deleted target and draft");
        }

        _api.Seed(_chat, PendingEntry("DISCARD_SYNTHETIC 普通候选"));
        NativeUi.Invoke(Button("RefreshMemoryButton"));
        await WaitAsync(() => !_viewModel.IsBusy && _viewModel.Entries.Count == 1, "native Refresh loads a candidate to discard");
        await OpenEntryAsync(0);
        before = _api.Writes;
        NativeUi.Invoke(Button("DeleteMemoryButton"));
        var discardDialog = await WaitForDialogAsync("native Discard explicitly describes pending candidate removal");
        Check(discardDialog.Title?.ToString() == UiText.Get("丢弃这条草稿？") && _api.Writes == before,
            "discard confirmation distinguishes a pending candidate from confirmed memory deletion");
        await DismissDialogAsync(discardDialog, primary: true);
        await WaitAsync(() => !_viewModel.IsBusy && _viewModel.IsNew && _viewModel.Entries.Count == 0,
            "native discard removes the candidate without confirming it");
        _api.Seed(_chat, PendingEntry("UNAVAILABLE_PENDING_SYNTHETIC") with { SourceAvailable = false });
        NativeUi.Invoke(Button("RefreshMemoryButton"));
        await WaitAsync(() => !_viewModel.IsBusy && _viewModel.Entries.Count == 1, "native Refresh loads an unavailable pending source");
        await OpenEntryAsync(0);
        Check(!Button("ConfirmMemoryButton").IsEnabled && Label("MemorySourceStatusLabel").Text.Contains(UiText.Get("来源不可用")),
            "unavailable user quotation remains visible and cannot be confirmed");
        await CaptureAsync("confirmation-pending-source");

        _api.Seed(_chat, PendingEntry("COMMITTED_CONFIRM_SYNTHETIC"));
        NativeUi.Invoke(Button("RefreshMemoryButton"));
        await WaitAsync(() => !_viewModel.IsBusy, "native Refresh loads the confirmation recovery fixture");
        await OpenEntryAsync(0);
        before = _api.Writes;
        _api.FailNextRead(_chat, new HttpRequestException("Synthetic confirmation refresh failure"));
        NativeUi.Invoke(Button("ConfirmMemoryButton"));
        await WaitAsync(() => !_viewModel.IsBusy && _viewModel.Status == MemoryManagementStatus.ConfirmedRefreshFailed,
            "native known committed confirmation distinguishes a failed status reload");
        Check(_api.Writes == before + 1 && !Button("ConfirmMemoryButton").IsEnabled && !Button("SaveMemoryButton").IsEnabled,
            "failed reload blocks duplicate confirmation and save until authoritative refresh");
        NativeUi.Invoke(Button("RefreshMemoryButton"));
        await WaitAsync(() => !_viewModel.IsBusy && _viewModel.Status == MemoryManagementStatus.Confirmed,
            "native Refresh reconciles the confirmed outcome without another write");
        Check(_api.Writes == before + 1 && !Button("ConfirmMemoryButton").IsEnabled,
            "confirmed recovery never repeats the already committed operation");

        _api.Seed(_chat, PendingEntry("LATE_CONFIRM_SYNTHETIC"));
        NativeUi.Invoke(Button("RefreshMemoryButton"));
        await WaitAsync(() => !_viewModel.IsBusy, "native Refresh loads the late confirmation fixture");
        await OpenEntryAsync(0);
        var delayedConfirmationRead = _api.DelayNextRead(_chat);
        NativeUi.Invoke(Button("ConfirmMemoryButton"));
        await WaitAsync(() => _viewModel.IsBusy, "confirmation status reload waits while the committed write is protected");
        NativeUi.Invoke(Button("MemoryScopeUserButton"));
        await WaitAsync(() => _viewModel.Target?.Scope == MemoryScopes.User && !_viewModel.IsBusy,
            "native scope change cancels the superseded confirmation reload");
        long? userRevision = _viewModel.ScopeRevision;
        delayedConfirmationRead.SetResult(_api.Snapshot(_chat) with { Revision = 999 });
        await SettleAsync();
        Check(_viewModel.Target?.Scope == MemoryScopes.User && _viewModel.ScopeRevision == userRevision &&
            !_viewModel.Entries.Any(entry => entry.Content.Contains("LATE_CONFIRM_SYNTHETIC")),
            "late confirmation status cannot overwrite another scope's entries or version");

        NativeUi.Invoke(Button("MemoryScopeChatButton"));
        await WaitAsync(() => _viewModel.Target == _chat && !_viewModel.IsBusy, "native chat scope reopens after cancellation");
        _api.Seed(_chat, FakeMemoryApi.Entry(_chat, "CHAT_ONLY synthetic source", fromMessage: true));
        NativeUi.Invoke(Button("RefreshMemoryButton"));
        await WaitAsync(() => !_viewModel.IsBusy, "confirmation fixture restores synthetic chat data for subsequent checks");
        NativeUi.Invoke(Button("NewMemoryButton"));
        await WaitAsync(() => _viewModel.IsNew && !_viewModel.HasChanges, "confirmation fixture leaves a clean editor");
        NativeUi.Invoke(Button("MemoryScopeUserButton"));
        await WaitAsync(() => _viewModel.Target?.Scope == MemoryScopes.User && !_viewModel.IsBusy,
            "confirmation fixture returns to the global scope without widening a pending candidate");

        if (Environment.GetCommandLineArgs().Contains("--confirmation-only", StringComparer.OrdinalIgnoreCase))
        {
            _api.Seed(_chat, PendingEntry("CLOSE_CONFIRM_SYNTHETIC"));
            NativeUi.Invoke(Button("MemoryScopeChatButton"));
            await WaitAsync(() => _viewModel.Target == _chat && !_viewModel.IsBusy, "short native fixture opens the close confirmation candidate");
            await OpenEntryAsync(0);
            var delayedCloseRead = _api.DelayNextRead(_chat);
            NativeUi.Invoke(Button("ConfirmMemoryButton"));
            await WaitAsync(() => _viewModel.IsBusy, "short native fixture waits for a committed confirmation reload before close");
            long? revisionAtClose = _viewModel.ScopeRevision;
            NativeUi.RequestClose(_window!);
            await WaitAsync(() => _closed && _api.IsDisposed, "native close cancels confirmation reload and disposes its isolated API");
            delayedCloseRead.SetResult(_api.Snapshot(_chat) with { Revision = 999, Entries = [] });
            await SettleAsync();
            Check(_viewModel.ScopeRevision == revisionAtClose && _viewModel.Entries.Count == 1,
                "late confirmation response after native close cannot mutate the disposed editor");
        }
    }

    private MemoryEntry PendingEntry(string content)
    {
        var entry = FakeMemoryApi.Entry(_chat, content, active: false, fromMessage: true);
        return entry with { Status = MemoryStatuses.Draft, Source = entry.Source with { ConversationId = _chat.Id },
            Candidate = new("conservative-extractive-v1", "decision", new string('a', 64),
                [new(entry.Source.MessageId!.Value, "SAVED_USER_QUOTE 原样用户来源")]) };
    }

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
        NativeUi.ById<SegmentedSelector>(_root, "MemoryKindPicker").SelectedKey = MemoryKinds.Decision;
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
        Check(NativeUi.Descendants<RadioButton>(NativeUi.ById<SegmentedSelector>(_root, "MemoryKindPicker"))
                .Where(button => button.IsChecked == true).SelectMany(NativeUi.Descendants<TextBlock>)
                .Any(label => NativeUi.IsVisible(label) && label.Text == UiText.Get("决定")),
            "live language change updates the visible selected kind caption in the native segmented selector");
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
