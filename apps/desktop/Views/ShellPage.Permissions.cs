using KYNXA.Contracts;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private void InitializePermissionPicker()
    {
        if (!ChatPermissionModes.IsSupported(_layout.PermissionMode)) _layout.PermissionMode = ChatPermissionModes.Ask;
        UpdatePermissionPickerLabel();
    }

    private void UpdatePermissionPickerLabel()
    {
        SelectedPermissionLabel.Text = "文本对话";
        SelectedPermissionLabel.Foreground = (Brush)Application.Current.Resources["KynxaSecondaryTextBrush"];
        SelectedPermissionIcon.Source = (ImageSource)Application.Current.Resources["IconPermissionAsk"];
        AutomationProperties.SetName(PermissionPickerButton, "当前能力：文本对话");
        ToolTipService.SetToolTip(PermissionPickerButton, "模型支持文本对话；工作文件可手动只读预览，工具操作与审批暂未开放");
    }

    private async void PermissionPickerButton_Click(object sender, RoutedEventArgs e) =>
        await ShowFeatureInfoAsync("当前能力", "目前可以与模型进行文本对话。工作详情中的“文件”可手动查看关联目录和预览小型文本，内容不会自动发送给模型。模型操作文件、联网工具和自动执行暂未开放。");
}
