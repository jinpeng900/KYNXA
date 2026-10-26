using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private Action? _refreshSettingsCloseNotice;

    private sealed record SettingsCloseEntry(string Category, string LabelKey, bool BlocksClose,
        IReadOnlyList<string> Changes, Action Reveal);

    private List<SettingsCloseEntry> SettingsCloseEntries()
    {
        var entries = new List<SettingsCloseEntry>();
        if (_memoryManagementWindow is { } memory)
            entries.Add(new("memory", "记忆", memory.BlocksSettingsClose,
                memory.HasUnsavedChanges ? memory.UnsavedChangeLabels : [], memory.RevealUnsavedChanges));
        if (_toolManagementWindow is { } tools)
            entries.Add(new("tools", "工具与技能", tools.BlocksSettingsClose,
                tools.HasUnsavedChanges ? tools.UnsavedChangeLabels : [], tools.RevealUnsavedChanges));
        if (_retrievalSettingsWindow is { } retrieval)
            entries.Add(new("retrieval", "检索与网页搜索", retrieval.BlocksSettingsClose,
                retrieval.HasUnsavedChanges ? retrieval.UnsavedChangeLabels : [], retrieval.RevealUnsavedChanges));
        return entries;
    }

    private void SettingsPendingStateChanged(object? sender, EventArgs args)
    {
        // A controller reports real state changes; the close hint needs no polling or background timer.
        // 控制器主动报告真实状态变化；关闭提示不使用轮询或后台定时器。
        if (DispatcherQueue.HasThreadAccess) _refreshSettingsCloseNotice?.Invoke();
        else DispatcherQueue.TryEnqueue(() => _refreshSettingsCloseNotice?.Invoke());
    }

    private ContentDialog CreateSettingsCloseDialog(FrameworkElement owner, IReadOnlyList<SettingsCloseEntry> entries)
    {
        var body = new StackPanel { Spacing = 14, MaxWidth = 420 };
        var explanation = new TextBlock { TextWrapping = TextWrapping.Wrap };
        UiLocalization.Bind(explanation, TextBlock.TextProperty, "以下内容尚未保存。继续编辑可保留草稿，放弃后将关闭设置。");
        body.Children.Add(explanation);
        var changes = new StackPanel { Spacing = 14 };
        foreach (var entry in entries)
        {
            var group = new StackPanel { Spacing = 6 };
            var heading = new TextBlock { FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, TextWrapping = TextWrapping.Wrap };
            UiLocalization.Bind(heading, TextBlock.TextProperty, entry.LabelKey);
            group.Children.Add(heading);
            foreach (string change in entry.Changes)
                group.Children.Add(new TextBlock { Text = "• " + change, FontSize = 13,
                    TextWrapping = TextWrapping.Wrap, Foreground = AppearanceService.GetBrush("KynxaSecondaryTextBrush") });
            changes.Children.Add(group);
        }
        body.Children.Add(new Border { Padding = new Thickness(14), CornerRadius = new CornerRadius(12),
            Background = AppearanceService.GetBrush("KynxaSettingsCardBrush"),
            BorderBrush = AppearanceService.GetBrush("KynxaDividerBrush"), BorderThickness = new Thickness(1),
            Child = new ScrollViewer { Content = changes, MaxHeight = 260,
                HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto } });
        var dialog = new ContentDialog { XamlRoot = owner.XamlRoot, Content = body,
            DefaultButton = ContentDialogButton.Close };
        // Escape and the default button keep editing. Only an explicit Primary result authorizes discarding drafts.
        // Escape 与默认按钮均保留编辑；只有显式点击主操作，才授权丢弃草稿。
        UiLocalization.Bind(dialog, ContentDialog.TitleProperty, "有未保存的修改");
        UiLocalization.Bind(dialog, ContentDialog.PrimaryButtonTextProperty, "放弃并关闭");
        UiLocalization.Bind(dialog, ContentDialog.CloseButtonTextProperty, "继续编辑");
        AutomationProperties.SetAutomationId(dialog, "SettingsCloseConfirmation");
        DialogPresentation.Prepare(dialog);
        dialog.CloseButtonStyle = (Style)Application.Current.Resources["KynxaDialogAccentButtonStyle"];
        return dialog;
    }
}
