using System.Net;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Microsoft.UI.Xaml.Controls;

namespace MemoryUiSmoke;

public partial class App
{
    private async Task CheckErrorsAndConflictsAsync()
    {
        NativeUi.SetText(ContentBox, "   ");
        await SettleAsync();
        Check(!Button("SaveMemoryButton").IsEnabled && Label("MemoryInputHint").Text.Contains("不能为空"),
            "native empty-content validation disables Save and explains the error");
        NativeUi.SetText(ContentBox, new string('x', MemoryInputValidation.MaximumContentLength + 1));
        await SettleAsync();
        Check(!Button("SaveMemoryButton").IsEnabled && Label("MemoryInputHint").Text.Contains("4000"),
            "native oversized-content validation follows the shared 4000-character contract");
        NativeUi.SetText(ContentBox, string.Empty);
        await SettleAsync();
        await OpenEntryAsync(0);
        string draft = "CONFLICT_DRAFT_KEEP 中文输入不可丢失";
        NativeUi.SetText(ContentBox, draft);
        await SettleAsync();
        int beforeConflict = _api.Writes;
        _api.ConflictNextWrite = true;
        await SaveAsync(MemoryManagementStatus.Conflict);
        Check(_api.Writes == beforeConflict && ContentBox.Text == draft && _viewModel.HasChanges &&
            _viewModel.Entries[0].Content.Contains("SERVER_CONCURRENT_EDIT") && Button("SaveMemoryButton").IsEnabled,
            "409 conflict refreshes the backend revision while preserving the exact pending editor input");
        Check(StatusBar.IsOpen && StatusBar.Severity == InfoBarSeverity.Warning && StatusBar.Message.Contains("保留"),
            "native conflict warning explains the preserved edit and requires a new user Save");
        await CaptureAsync("06-conflict-preserved-input");
        await SaveAsync();
        Check(_api.Writes == beforeConflict + 1 && _api.Snapshot(_user).Entries[0].Content == draft,
            "a second explicit Save commits against the refreshed backend revision");

        draft = "WRITE_FAILURE_DRAFT_KEEP 临时故障保持输入";
        NativeUi.SetText(ContentBox, draft);
        await SettleAsync();
        _api.NextWriteFailure = new GatewayApiException("Synthetic unavailable gateway", HttpStatusCode.ServiceUnavailable, "SYNTHETIC_UNAVAILABLE");
        await SaveAsync(MemoryManagementStatus.Error);
        Check(ContentBox.Text == draft && StatusBar.IsOpen && StatusBar.Severity == InfoBarSeverity.Error,
            "native write failure presents an error and retains the pending editor input");
        await SaveAsync();
        Check(_api.Snapshot(_user).Entries[0].Content == draft, "native Save recovers after the synthetic gateway failure");

        _api.FailNextRead(_user, new InvalidDataException("Synthetic memory read failure"));
        NativeUi.Invoke(Button("RefreshMemoryButton"));
        await WaitAsync(() => !_viewModel.IsBusy && _viewModel.Status == MemoryManagementStatus.Error,
            "native Refresh presents a backend read failure");
        Check(!_viewModel.IsLoaded && !Button("SaveMemoryButton").IsEnabled && StatusBar.IsOpen &&
            StatusBar.Severity == InfoBarSeverity.Error,
            "failed read cannot be mistaken for a writable empty snapshot");
        await CaptureAsync("07-read-error");
        NativeUi.Invoke(Button("RefreshMemoryButton"));
        await WaitAsync(() => _viewModel.IsLoaded && !_viewModel.IsBusy && !StatusBar.IsOpen,
            "native Refresh recovers from the read failure using a new backend snapshot");
    }
}
