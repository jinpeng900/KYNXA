using KYNXA.Contracts;
using KYNXA_Desktop;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Graphics;

namespace MemoryUiSmoke;

public partial class App
{
    private async Task CheckTargetsAsync()
    {
        NativeUi.Invoke(Button("MemoryScopeChatButton"));
        await WaitAsync(() => _viewModel.Target?.Id == _chat.Id && _viewModel.IsLoaded && !_viewModel.IsBusy,
            "native Chat scope loads only the selected saved chat memory");
        Check(EntryList.Items.Count == 1 && EntryFromItem(EntryList.Items[0]).Content.Contains("CHAT_ONLY") &&
            ContextPicker.Items.Count == 2 && Label("MemoryScopeLabel").Text == _chat.DisplayName,
            "native chat context picker preserves synthetic names and scope isolation");
        ContextPicker.SelectedItem = _emptyChat;
        await WaitAsync(() => _viewModel.Target?.Id == _emptyChat.Id && _viewModel.IsLoaded && !_viewModel.IsBusy,
            "native context selection loads the empty saved chat scope");
        Check(EntryList.Items.Count == 0 && NativeUi.IsVisible(NativeUi.ByName<FrameworkElement>(_root, "MemoryEmptyPanel")) &&
            Label("MemoryEmptyLabel").Text == UiText.Get("暂无记忆，点击新增记忆。") && Button("NewMemoryButton").IsEnabled,
            "loaded empty scope shows its empty-state guidance and permits New");
        await CaptureAsync("08-empty-chat");
        _api.FailNextRead(_emptyChat, new InvalidDataException("Synthetic empty-scope read failure"));
        NativeUi.Invoke(Button("RefreshMemoryButton"));
        await WaitAsync(() => _viewModel.Status == MemoryManagementStatus.Error && !_viewModel.IsBusy,
            "native empty-scope Refresh can display an error");
        Check(Label("MemoryEmptyLabel").Text == UiText.Get("未能读取记忆，请刷新重试。") && !Button("NewMemoryButton").IsEnabled,
            "native empty-scope read error uses failure guidance instead of successful-empty guidance");
        NativeUi.Invoke(Button("RefreshMemoryButton"));
        await WaitAsync(() => _viewModel.IsLoaded && !_viewModel.IsBusy, "native empty-scope Refresh recovers");

        var delayedProject = _api.DelayNextRead(_project);
        NativeUi.Invoke(Button("MemoryScopeProjectButton"));
        await WaitAsync(() => _viewModel.Target?.Id == _project.Id && _viewModel.IsBusy,
            "native Work scope exposes a pending backend load");
        Check(Button("MemoryScopeUserButton").IsEnabled && ContextPicker.IsEnabled && !Button("NewMemoryButton").IsEnabled,
            "target controls remain usable during loading while mutation controls are disabled");
        NativeUi.Invoke(Button("MemoryScopeUserButton"));
        await WaitAsync(() => _viewModel.Target?.Scope == MemoryScopes.User && _viewModel.IsLoaded && !_viewModel.IsBusy,
            "native scope change supersedes the pending Work load");
        delayedProject.SetResult(_api.Snapshot(_project));
        await SettleAsync();
        Check(_viewModel.Target?.Scope == MemoryScopes.User && EntryList.Items.Count == 4 &&
            _viewModel.Entries.All(entry => entry.Scope == MemoryScopes.User),
            "a cancelled Work request returning late cannot replace the active global list");

        NativeUi.Invoke(Button("MemoryScopeChatButton"));
        await WaitAsync(() => _viewModel.Target?.Id == _chat.Id && !_viewModel.IsBusy, "native Chat scope reopens its saved target");
        var delayedEmpty = _api.DelayNextRead(_emptyChat);
        ContextPicker.SelectedItem = _emptyChat;
        await WaitAsync(() => _viewModel.Target?.Id == _emptyChat.Id && _viewModel.IsBusy,
            "native context picker starts a pending target load");
        ContextPicker.SelectedItem = _chat;
        await WaitAsync(() => _viewModel.Target?.Id == _chat.Id && _viewModel.IsLoaded && !_viewModel.IsBusy,
            "native context picker can supersede a pending target load");
        delayedEmpty.SetResult(_api.Snapshot(_emptyChat));
        await SettleAsync();
        Check(_viewModel.Target?.Id == _chat.Id && EntryList.Items.Count == 1 && _viewModel.Entries[0].Content.Contains("CHAT_ONLY"),
            "a late context-picker response cannot erase the newly selected chat memory");

        NativeUi.Invoke(Button("MemoryScopeProjectButton"));
        await WaitAsync(() => _viewModel.Target?.Id == _project.Id && _viewModel.IsLoaded && !_viewModel.IsBusy,
            "native Work scope reloads after the superseded request");
        Check(EntryList.Items.Count == 1 && EntryFromItem(EntryList.Items[0]).Content.Contains("PROJECT_ONLY") &&
            Label("MemoryScopeLabel").Text == _project.DisplayName,
            "native Work scope uses stable project identity and preserves its custom display name");
        UiText.Initialize("en");
        await SettleAsync();
        Check(ContextPicker.SelectedItem is MemoryTarget selectedProject && selectedProject == _project && Label("MemoryScopeLabel").Text == _project.DisplayName,
            "live language switch retains the selected project and its custom name");
        UiText.Initialize("zh-CN");
        await SettleAsync();
        ContextPicker.SelectedItem = _archivedProject;
        await WaitAsync(() => _viewModel.Target?.Id == _archivedProject.Id && _viewModel.IsLoaded && !_viewModel.IsBusy,
            "native context picker opens an archived project memory scope");
        Check(Label("MemoryScopeLabel").Text.Contains("工作已归档，共享记忆暂停用于聊天。") &&
            ((MemoryDisplayRow)EntryList.Items[0]).Detail.Contains("工作已归档"),
            "archived project metadata explains that shared memory is paused for chats");
        await OpenEntryAsync(0);
        NativeUi.SetText(ContentBox, "ARCHIVED_PROJECT_EDIT synthetic confirmed update");
        await SettleAsync();
        await SaveAsync();
        Check(_api.Snapshot(_archivedProject).Entries[0].Content.Contains("ARCHIVED_PROJECT_EDIT") &&
            Label("MemorySourceStatusLabel").Text.Contains("工作已归档"),
            "archived project permits intentional memory editing while preserving its paused-sharing status");
        NativeUi.Invoke(Button("NewMemoryButton"));
        await SettleAsync();
        NativeUi.SetText(ContentBox, "ARCHIVED_PROJECT_CREATE synthetic manual memory");
        await SettleAsync();
        await SaveAsync();
        Check(_api.Snapshot(_archivedProject).Entries.Length == 2, "archived project permits intentional manual memory creation");
        await CaptureAsync("08b-archived-project");
        NativeUi.Invoke(Button("DeleteMemoryButton"));
        var archivedDelete = await WaitForDialogAsync("archived project Delete retains its confirmation boundary");
        await DismissDialogAsync(archivedDelete, primary: true);
        await WaitAsync(() => _viewModel.Status == MemoryManagementStatus.Deleted && !_viewModel.IsBusy,
            "archived project permits confirmed deletion of its selected memory");
        Check(_api.Snapshot(_archivedProject).Entries.Length == 1 && _api.Snapshot(_project).Entries.Length == 1,
            "archived-project CRUD remains isolated from the active project's memory");
        NativeUi.Invoke(Button("MemoryScopeUserButton"));
        await WaitAsync(() => _viewModel.Target?.Scope == MemoryScopes.User && _viewModel.IsLoaded && !_viewModel.IsBusy,
            "native Global scope returns to the confirmed global snapshot");
    }

    private async Task CheckWindowLifecycleAsync()
    {
        await OpenEntryAsync(0);
        Guid selectedId = _viewModel.SelectedEntry!.Id;
        string content = ContentBox.Text;
        string language = UiText.Language;
        UiText.Initialize("en");
        foreach (int width in new[] { 600, 960, 1600 })
        {
            _window!.AppWindow.Resize(new SizeInt32(width, 900));
            await SettleAsync();
            CheckControlBounds(width);
            foreach (string id in new[] { "MemoryScopeChatButton", "MemoryScopeProjectButton", "MemoryScopeUserButton" })
            {
                var button = Button(id);
                var label = (TextBlock)button.Content;
                var labelBounds = label.TransformToVisual(button).TransformBounds(new Windows.Foundation.Rect(
                    0, 0, label.ActualWidth, label.ActualHeight));
                Check(labelBounds.Right <= button.ActualWidth + 1 && labelBounds.Bottom <= button.ActualHeight + 1 &&
                    Microsoft.UI.Xaml.Automation.AutomationProperties.GetName(button) == label.Text,
                    "localized memory scope text and accessible name remain complete at width " + width);
            }
            Check(ContentBox.Text == content && _viewModel.SelectedEntry?.Id == selectedId,
                "native resize preserves the selected entry and editor text at width " + width);
            await CaptureAsync("09-layout-" + width);
        }
        UiText.Initialize(language);
        await SettleAsync();
        var presenter = (OverlappedPresenter)_window!.AppWindow.Presenter;
        presenter.Minimize(false);
        await WaitAsync(() => presenter.State == OverlappedPresenterState.Minimized, "native memory window minimizes");
        presenter.Restore(false);
        await WaitAsync(() => presenter.State == OverlappedPresenterState.Restored, "native memory window restores from minimized state");
        Check(ContentBox.Text == content && _viewModel.SelectedEntry?.Id == selectedId,
            "native minimize and restore preserve editor state");
        presenter.Maximize();
        await WaitAsync(() => presenter.State == OverlappedPresenterState.Maximized, "native memory window maximizes");
        CheckControlBounds(_window.AppWindow.Size.Width);
        presenter.Restore(false);
        await WaitAsync(() => presenter.State == OverlappedPresenterState.Restored, "native memory window restores from maximized state");
        _window.AppWindow.Resize(new SizeInt32(1120, 860));
        await SettleAsync();
        await CaptureAsync("10-restored-window");

        var delayedClose = _api.DelayNextRead(_user);
        NativeUi.Invoke(Button("RefreshMemoryButton"));
        await WaitAsync(() => _viewModel.IsBusy, "native Refresh begins before closing its owner window");
        int countAtClose = _viewModel.Entries.Count;
        long? revisionAtClose = _viewModel.ScopeRevision;
        NativeUi.RequestClose(_window);
        await WaitAsync(() => _closed && _api.IsDisposed, "closing the native memory window disposes the injected API");
        delayedClose.SetResult(_api.Snapshot(_user) with { Revision = 999, Entries = [] });
        await Task.Delay(150);
        Check(_viewModel.Entries.Count == countAtClose && _viewModel.ScopeRevision == revisionAtClose,
            "a late read after native window close cannot mutate the disposed editor");
    }

    private async Task CheckGlobalOnlyWindowAsync()
    {
        using var api = new FakeMemoryApi();
        api.Seed(_user);
        var viewModel = new MemoryManagementViewModel(api);
        var window = new MemoryManagementWindow([], viewModel: viewModel);
        var root = (FrameworkElement)window.Content;
        window.Activate();
        try
        {
            await WaitAsync(() => viewModel.Target?.Scope == MemoryScopes.User && viewModel.IsLoaded && !viewModel.IsBusy,
                "a native window with no saved targets still opens the global memory scope");
            Check(NativeUi.ById<Button>(root, "NewMemoryButton").IsEnabled &&
                NativeUi.ById<ListView>(root, "MemoryEntryList").Items.Count == 0,
                "global memory remains accessible without a saved chat or real project");
            NativeUi.Invoke(NativeUi.ById<Button>(root, "MemoryScopeChatButton"));
            await WaitAsync(() => viewModel.Target is null && NativeUi.ByName<TextBlock>(root, "MemoryEmptyLabel").Text ==
                UiText.Get("发送消息后，即可管理这个聊天的记忆。"), "native missing-chat scope explains the saved-chat requirement");
            Check(!NativeUi.ById<Button>(root, "NewMemoryButton").IsEnabled && api.Writes == 0,
                "opening a missing saved-chat target creates no memory or authoritative fixture data");
        }
        finally { window.CloseForOwner(); }
    }
}
