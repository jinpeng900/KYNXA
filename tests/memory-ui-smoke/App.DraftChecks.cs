namespace MemoryUiSmoke;

public partial class App
{
    private bool _draftOnly;

    // Keep the existing focused entry while reusing the current explicit draft-review workflow.
    // 保留已有专项入口，复用当前明确的草稿审核流程，避免旧只读限制覆盖新确认功能。
    private void SeedDraftScenario() => _draftOnly = Environment.GetCommandLineArgs().Contains("--draft-only", StringComparer.Ordinal);

    private Task CheckDraftOnlyAsync() => CheckDraftConfirmationAsync();
}
