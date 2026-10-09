using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace RetrievalUiSmoke;

public partial class App
{
    private bool HasIndexText(string text) => NativeUi.Descendants<TextBlock>(_root)
        .Any(label => label.Text.Contains(text, StringComparison.Ordinal));

    private async Task VerifyJobStatesAsync()
    {
        int initialRebuilds = _api.Rebuilds;
        int initialCancellations = _api.CancelledJobs;
        _api.Job = new("synthetic_checkpoint", "paused", 3, 20);
        _api.StatusJobs = [_api.Job];
        NativeUi.Invoke(Button("RetrievalRefresh"));
        await WaitAsync(() => HasIndexText("索引任务已暂停，等待恢复") && !_window!.HasPendingChanges,
            "A paused checkpoint returned by status remains visible after refresh.");
        Check(Button("RetrievalCancelIndex").Visibility == Visibility.Visible && !Button("RetrievalRebuildIndex").IsEnabled && !Notice.IsOpen,
            "Paused indexing remains cancellable without offering a duplicate rebuild or a failure notice.");

        // Change only the fake server receipt; the real DispatcherTimer must discover the transition.
        // 只改变虚构服务端回执，由真实 DispatcherTimer 发现状态转换，不直接调用界面私有方法。
        int readsBeforeRecovery = _api.JobReads;
        _api.Job = _api.Job with { Status = "running", CompletedSources = 7 };
        await WaitAsync(() => _api.JobReads > readsBeforeRecovery && HasIndexText("正在更新索引 · 7/20"),
            "A paused job keeps polling and observes gateway-owned recovery to running.");
        _api.Job = _api.Job with { Status = "partial", CompletedSources = 20,
            Coverage = new RetrievalCoverageStatus(20, 19, 15, 1, 0, 4, false) };
        await WaitAsync(() => HasIndexText("索引部分完成") && HasIndexText("关键词覆盖 19") && HasIndexText("语义覆盖 15"),
            "Partial completion reports actual lexical and semantic coverage instead of claiming complete success.");
        Check(HasIndexText("失败 1") && HasIndexText("跳过 0") && HasIndexText("部分 4") && !HasIndexText("索引更新失败，请重试") && !Notice.IsOpen,
            "Partial coverage preserves gap counts without turning the receipt into a generic failure.");
        Check(Button("RetrievalRebuildIndex").IsEnabled && Button("RetrievalCancelIndex").Visibility == Visibility.Collapsed,
            "Partial is terminal: cancellation is hidden and a deliberate later rebuild is available.");

        // The stored status snapshot still says paused; localization must retain the newer polled partial receipt.
        // 已保存状态快照仍为暂停；语言切换必须保留随后轮询取得的部分完成回执。
        UiText.Initialize("en");
        await WaitAsync(() => HasIndexText("Index partially completed") && HasIndexText("Keyword coverage 19") && HasIndexText("Semantic coverage 15"),
            "Language changes preserve and translate the newer polled partial receipt.");
        Check(!HasIndexText("awaiting recovery") && Button("RetrievalRebuildIndex").IsEnabled,
            "An older paused snapshot cannot revert the displayed terminal state during localization.");
        await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "partial-job-en.png"));
        int terminalReads = _api.JobReads;
        await Task.Delay(1250);
        Check(_api.JobReads == terminalReads, "Terminal partial completion stops background job polling.");

        UiText.Initialize("zh-CN");
        _api.Job = _api.Job with { Coverage = null, CompletedSources = 18 };
        _api.StatusJobs = [_api.Job];
        NativeUi.Invoke(Button("RetrievalRefresh"));
        await WaitAsync(() => HasIndexText("索引部分完成 · 已处理 18/20 个来源") && !_window!.HasPendingChanges,
            "Partial status without optional coverage stays readable and labels processed sources honestly.");

        _api.Job = new("synthetic_cancel_paused", "paused", 2, 20);
        _api.StatusJobs = [_api.Job];
        NativeUi.Invoke(Button("RetrievalRefresh"));
        await WaitAsync(() => HasIndexText("索引任务已暂停，等待恢复") && !_window!.HasPendingChanges,
            "A later paused job is discovered through the normal refresh path.");
        UiText.Initialize("en");
        await WaitAsync(() => HasIndexText("Indexing paused, awaiting recovery"), "Paused state changes language immediately.");
        NativeUi.Invoke(Button("RetrievalCancelIndex"));
        await WaitAsync(() => _api.CancelledJobs == initialCancellations + 1 && HasIndexText("Indexing cancelled") && !_window!.HasPendingChanges,
            "The existing cancel action cancels a paused checkpoint through its owned job ID.");
        Check(_api.LastCancelledJobId == "synthetic_cancel_paused" && Button("RetrievalRebuildIndex").IsEnabled &&
            Button("RetrievalCancelIndex").Visibility == Visibility.Collapsed,
            "Paused cancellation keeps job identity and restores the idle controls.");
        Check(_api.Rebuilds == initialRebuilds, "Polling, localization and cancellation never start or resume indexing themselves.");

        // An explicit empty status response clears stale controls instead of retaining a phantom checkpoint.
        // 显式空状态响应清除旧任务展示，避免保留已不存在检查点的取消入口。
        _api.StatusJobs = [];
        NativeUi.Invoke(Button("RetrievalRefresh"));
        await WaitAsync(() => HasIndexText("2 sources · 8 indexed chunks") && !_window!.HasPendingChanges,
            "Refreshing an empty job list restores source counts without a stale task status.");
        Check(Button("RetrievalRebuildIndex").IsEnabled && Button("RetrievalCancelIndex").Visibility == Visibility.Collapsed,
            "An empty job list leaves no cancellable phantom checkpoint.");
        UiText.Initialize("zh-CN");
    }
}
