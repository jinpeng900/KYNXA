using KYNXA_Desktop.Services;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop;

public sealed partial class ModelManagementWindow
{
    private bool _busy;
    private bool _confirmingDiscard;
    private bool _allowClose;
    private ModelPreset? _appliedPreset;
    private EditorState? _savedForm;
    private ContentDialog? _discardDialog;
    private string _discardAction = "";

    // Track raw token drafts and automatic/manual context mode without storing any secret.
    // 记录 token 原始草稿及上下文自动/手动模式，不将密钥存入快照。
    private sealed record EditorState(string Name, string Id, string Address, string Models, string Protocol,
        string ContextChoice, string CustomContext, string OutputChoice, string CustomOutput, bool AutomaticContext);

    private EditorState CurrentForm() => new(NameBox.Text, ProviderIdBox.Text, BaseUrlBox.Text,
        string.Join("\n", ModelIds()), (ProtocolBox.SelectedItem as ComboBoxItem)?.Tag?.ToString() ?? "",
        (ContextWindowBox.SelectedItem as ComboBoxItem)?.Tag?.ToString() ?? "", CustomContextWindowBox.Text,
        (MaxOutputTokensBox.SelectedItem as ComboBoxItem)?.Tag?.ToString() ?? "", CustomMaxOutputTokensBox.Text,
        _automaticContextWindow);

    private void CaptureSavedForm()
    {
        _savedForm = CurrentForm();
        UpdateDraftPresentation();
    }

    private bool HasUnsavedChanges() => _savedForm is not null &&
        (_savedForm != CurrentForm() || !string.IsNullOrWhiteSpace(ApiKeyBox.Password));

    private void InitializeDraftPresentation()
    {
        NameBox.TextChanged += (_, _) => UpdateDraftPresentation();
        ProviderIdBox.TextChanged += (_, _) => UpdateDraftPresentation();
        BaseUrlBox.TextChanged += (_, _) => UpdateDraftPresentation();
        ModelsBox.TextChanged += (_, _) => UpdateDraftPresentation();
        CustomContextWindowBox.TextChanged += (_, _) => UpdateDraftPresentation();
        CustomMaxOutputTokensBox.TextChanged += (_, _) => UpdateDraftPresentation();
        ApiKeyBox.PasswordChanged += (_, _) => UpdateDraftPresentation();
        ProtocolBox.SelectionChanged += (_, _) => UpdateDraftPresentation();
        ContextWindowBox.SelectionChanged += (_, _) => UpdateDraftPresentation();
        MaxOutputTokensBox.SelectionChanged += (_, _) => UpdateDraftPresentation();
    }

    private void UpdateDraftPresentation()
    {
        if (_closed || DraftStateText is null) return;
        // This badge describes the editable configuration only; the operation footer owns busy and result feedback.
        // 此标签仅说明当前配置草稿；忙碌及操作结果仍由底部区域反馈。
        string key = HasUnsavedChanges() ? "未保存的修改" : _editing is null ? "尚未保存" : "已保存";
        DraftStateText.Text = UiText.Get(key);
        DraftStateBadge.Visibility = _busy ? Visibility.Collapsed : Visibility.Visible;
        ToolTipService.SetToolTip(EditorTitle, EditorTitle.Text);
    }

    private async Task BeginNewConnectionAsync()
    {
        if (_busy || _closed) return;
        if (await ConfirmDiscardAsync("添加连接")) ApplyPreset(ModelPresets.All[0]);
    }

    private async Task<bool> ConfirmDiscardAsync(string action)
    {
        if (_closed || _confirmingDiscard) return false;
        if (!HasUnsavedChanges()) return true;
        if (WindowRoot.XamlRoot is null) return false;
        _confirmingDiscard = true;
        _discardAction = action;
        var dialog = new ContentDialog
        {
            XamlRoot = WindowRoot.XamlRoot,
            DefaultButton = ContentDialogButton.Close,
            PrimaryButtonStyle = (Style)Application.Current.Resources["KynxaQuietButtonStyle"],
            CloseButtonStyle = (Style)Application.Current.Resources["KynxaQuietButtonStyle"]
        };
        _discardDialog = dialog;
        UpdateDiscardDialogLanguage();
        try { return await dialog.ShowAsync() == ContentDialogResult.Primary && !_closed; }
        catch (Exception)
        {
            if (!_closed) SetStatus("暂时无法显示确认窗口，请关闭其他弹窗后重试。", true);
            return false;
        }
        finally { _discardDialog = null; _confirmingDiscard = false; }
    }

    private void UpdateDiscardDialogLanguage()
    {
        if (_discardDialog is not { } dialog) return;
        dialog.Title = UiText.Get("还有未保存的修改");
        dialog.Content = string.Format(UiText.Get("{0}会丢弃当前表单的修改，包括密钥、模型选择和 token 设置。可返回后先保存连接。"),
            UiText.Get(_discardAction));
        dialog.PrimaryButtonText = UiText.Get("丢弃修改");
        dialog.CloseButtonText = UiText.Get("继续编辑");
    }

    private async void ModelWindow_Closing(AppWindow sender, AppWindowClosingEventArgs args)
    {
        if (_closed || _allowClose || !HasUnsavedChanges()) return;
        // Keep the native close request cancelled while the discard decision is pending.
        // 丢弃决策尚未结束时，持续取消原生关闭请求，保留当前编辑。
        args.Cancel = true;
        if (!await ConfirmDiscardAsync("关闭窗口")) return;
        _allowClose = true;
        Close();
    }
}
