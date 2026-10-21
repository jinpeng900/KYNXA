using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml.Controls;

namespace RetrievalUiSmoke;

public partial class App
{
    private string EmbeddingText => NativeUi.ById<TextBlock>(_root, "RetrievalEmbeddingStatus").Text;

    private async Task VerifyEmbeddingStatesAsync()
    {
        int settingsReads = _api.SettingsReads;
        int globalWrites = _api.GlobalWrites, projectWrites = _api.ProjectWrites;
        _api.StatusJobs = [];
        await ObserveAsync(new("unloaded", "builtin-multilingual", 384, true, Loaded: false), "模型未加载",
            "A model with complete assets is explicitly unloaded, not reported as misconfigured or failed.");
        await ObserveAsync(new("waiting", "builtin-multilingual", 384, true, Loaded: false,
                Phase: "resource-admission", ElapsedMs: 23000, Reason: "resource-busy"), "已用时 23 秒",
            "Background resource waiting shows actual elapsed time while lexical retrieval remains available.");
        Check(EmbeddingText.Contains("前台任务优先") && !EmbeddingText.Contains('%'),
            "Resource waiting explains foreground priority without invented progress percentages.");
        await ObserveAsync(new("loading", "builtin-multilingual", 384, true, Loaded: false,
                Phase: "model-loading", ElapsedMs: 7000, Reason: "model-loading"), "模型加载中",
            "The live view observes model loading without requiring an index job or a manual refresh.");
        Check(EmbeddingText.Contains("已用时 7 秒") && EmbeddingText.Contains("使用关键词检索"),
            "Loading displays the real observed duration and retains keyword fallback.");
        await ObserveAsync(new("error", "builtin-multilingual", 384, true, "Synthetic invalid model asset",
                Loaded: false, ErrorCode: "EMBEDDING_ASSET_INVALID", Reason: "load-failed"), "模型加载失败",
            "Actual model loading failure is distinct from unloaded and waiting states.");
        Check(EmbeddingText.Contains("Synthetic invalid model asset"), "Load failure preserves the gateway's concrete diagnostic.");
        await ObserveAsync(new("unavailable", "builtin-multilingual", 384, false, Loaded: false,
                Reason: "assets-missing"), "缺少检索模型文件",
            "Missing model assets are identified separately from a failed load.");
        await ObserveAsync(new("ready", "builtin-multilingual", 384, true, Loaded: true), "本地就绪",
            "Background completion automatically updates the visible view to ready.");
        Check(!EmbeddingText.Contains("使用关键词检索") && !EmbeddingText.Contains("23"),
            "Ready clears stale loading, waiting duration and fallback descriptions.");
        UiText.Initialize("en");
        Check(EmbeddingText == "Ready locally", "The current ready observation changes language immediately.");
        UiText.Initialize("zh-CN");

        _api.StatusFailures = 1;
        await WaitAsync(() => EmbeddingText.Contains("状态暂时无法刷新"),
            "A transient status transport error retains the last known model state and schedules another observation.");
        Check(EmbeddingText.StartsWith("本地就绪", StringComparison.Ordinal) && !Notice.IsOpen,
            "A failed observation does not misreport model load failure or interrupt the settings view.");
        await WaitAsync(() => EmbeddingText == "本地就绪", "The next successful observation clears the transient status notice.");

        int readsBeforePending = _api.StatusReads;
        _api.StatusGate = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        _api.IgnoreStatusCancellation = true;
        _api.Embedding = new("loading", "builtin-multilingual", 384, true, Loaded: false, ElapsedMs: 55000);
        await WaitAsync(() => _api.StatusReads > readsBeforePending, "A background observation can remain pending independently of the form.");
        Check(Picker("RetrievalSemanticMode").IsEnabled && !_window!.HasPendingChanges,
            "A slow status response does not disable settings controls or enter the save barrier.");
        string beforeDetach = EmbeddingText;
        int readsAtDetach = _api.StatusReads;
        _window!.Content = new Grid();
        await WaitAsync(() => !_root.IsLoaded, "Detaching an embedded-capable settings root unloads its observation owner.");
        _api.StatusGate.SetResult();
        _api.StatusGate = null;
        _api.IgnoreStatusCancellation = false;
        await Task.Delay(2200);
        Check(_api.StatusReads == readsAtDetach && EmbeddingText == beforeDetach,
            "An unloaded view stops status polling and ignores a late response even when transport ignores cancellation.");
        _api.Embedding = new("waiting", "builtin-multilingual", 384, true, Loaded: false, Reason: "resource-busy");
        _window.Content = _root;
        await WaitAsync(() => _root.IsLoaded && EmbeddingText.Contains("等待资源"),
            "Reattaching the same view resumes observations and discovers the current state.");
        Check(_api.SettingsReads == settingsReads && _api.GlobalWrites == globalWrites && _api.ProjectWrites == projectWrites,
            "Background observation and reattachment neither reload form settings nor perform configuration writes.");
        await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "embedding-waiting-zh.png"));

        async Task ObserveAsync(RetrievalEmbeddingStatus status, string text, string description)
        {
            int before = _api.StatusReads;
            _api.Embedding = status;
            await WaitAsync(() => _api.StatusReads > before && EmbeddingText.Contains(text, StringComparison.Ordinal), description);
        }
    }
}
