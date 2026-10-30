using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop;

public sealed partial class ModelManagementWindow
{
    private async void DeleteConnectionButton_Click(object sender, RoutedEventArgs e)
    {
        if (_closed || _busy || !_loaded || _confirmingDiscard || _editing is not { } provider) return;
        SetBusy(true);
        SetStatus("正在删除连接…");
        try
        {
            await _api.DeleteAsync(provider.ProviderId, _lifetime.Token);
            if (_closed) return;
            // Apply the confirmed deletion without a second list request or touching model files.
            // 收到删除回执再更新列表，不重复请求，也不删除模型文件。
            _providers = _providers.Where(candidate => candidate.ProviderId != provider.ProviderId).ToArray();
            if (_providers.FirstOrDefault() is { } next) SelectProvider(next);
            else ApplyPreset(ModelPresets.All[0]);
            SetStatus(() => string.Format(UiText.Get("已删除连接“{0}”。"), provider.DisplayName));
        }
        catch (OperationCanceledException) when (_lifetime.IsCancellationRequested) { }
        catch (Exception error)
        {
            if (!_closed) SetStatus(() => string.Format(UiText.Get("删除失败：{0}"), FriendlyError(error)), true);
        }
        finally { if (!_closed) SetBusy(false); }
    }

    private void DismissStatusButton_Click(object sender, RoutedEventArgs e) => DismissStatusError();

    private void WindowRoot_PointerPressed(object sender, PointerRoutedEventArgs args)
    {
        if (!_statusIsError) return;
        for (DependencyObject? element = args.OriginalSource as DependencyObject; element is not null;
            element = VisualTreeHelper.GetParent(element))
            if (ReferenceEquals(element, StatusRow)) return;
        DismissStatusError();
    }

    private void DismissStatusError()
    {
        if (_statusIsError) SetStatus(() => string.Empty);
    }
}
