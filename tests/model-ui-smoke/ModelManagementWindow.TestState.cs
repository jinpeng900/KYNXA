namespace KYNXA_Desktop;

// Fixture-only access to lifecycle state. This partial is compiled only into ModelUiSmoke.
// 仅供夹具观察生命周期状态；此 partial 只编译到 ModelUiSmoke，不加入生产程序。
public sealed partial class ModelManagementWindow
{
    internal bool IsDiscardConfirmationPendingForTest => _confirmingDiscard;
    internal bool IsModelOperationBusyForTest => _busy;
    internal bool HasModelDraftForTest => HasUnsavedChanges();
    internal string? EditingProviderIdForTest => _editing?.ProviderId;
}
