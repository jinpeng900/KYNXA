using System.Text.Json;
using KYNXA.Contracts;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Services;

/// <summary>Displays the requested operation verbatim; the caller owns approval identity and cancellation.</summary>
public static class ToolApprovalDialog
{
    public static ContentDialog Create(XamlRoot root, ToolActivity tool)
    {
        var content = new StackPanel { Spacing = 10 };
        content.Children.Add(new TextBlock { Name = "ToolApprovalName", Text = tool.Name, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
        content.Children.Add(new TextBlock { Text = tool.Summary, TextWrapping = TextWrapping.Wrap });
        if (tool.Name.StartsWith("mcp.", StringComparison.Ordinal))
            AddLabel(content, "MCP 服务是受信任的外部程序，不属于终端沙箱。", "ToolApprovalExternalProgram");
        if (tool.Arguments is { ValueKind: JsonValueKind.Object } wrapped &&
            wrapped.TryGetProperty("policy", out var policy) && policy.ValueKind == JsonValueKind.Object &&
            policy.TryGetProperty("reason", out var reason) && reason.ValueKind == JsonValueKind.String)
        {
            AddLabel(content, "审批原因");
            content.Children.Add(new TextBlock { Name = "ToolApprovalReason", Text = reason.GetString(), TextWrapping = TextWrapping.Wrap });
        }
        AddLabel(content, "工作范围");
        if (tool.WorkspaceRoot is { } workspace)
            content.Children.Add(new TextBlock { Name = "ToolApprovalWorkspace", Text = workspace, TextWrapping = TextWrapping.Wrap });
        else AddLabel(content, "范围未提供", "ToolApprovalWorkspace");
        AddLabel(content, tool.OutsideWorkspace is true ? "此操作超出工作目录。" :
            tool.OutsideWorkspace is false ? "此操作位于工作目录内。" : "此工具未提供本地文件范围。", "ToolApprovalScope");
        AddLabel(content, "沙箱");
        if (tool.Sandbox is { } sandbox)
            content.Children.Add(new TextBlock { Name = "ToolApprovalSandbox", Text = sandbox, TextWrapping = TextWrapping.Wrap });
        else AddLabel(content, "沙箱信息未提供", "ToolApprovalSandbox");
        var parameters = new TextBox { Name = "ToolApprovalArguments", IsReadOnly = true, AcceptsReturn = true,
            TextWrapping = TextWrapping.Wrap, MaxHeight = 240,
            Text = tool.Arguments is { } arguments ? JsonSerializer.Serialize(arguments,
                new JsonSerializerOptions { WriteIndented = true }) : "{}" };
        parameters.Resources["TextControlBorderBrushFocused"] = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 136, 136, 136));
        content.Children.Add(parameters);
        var dialog = new ContentDialog { XamlRoot = root, Content = new ScrollViewer { Content = content, MaxHeight = 460 },
            DefaultButton = ContentDialogButton.None,
            PrimaryButtonStyle = (Style)Application.Current.Resources["KynxaQuietButtonStyle"],
            CloseButtonStyle = (Style)Application.Current.Resources["KynxaQuietButtonStyle"] };
        UiLocalization.Bind(dialog, ContentDialog.TitleProperty, "批准这次工具操作？");
        UiLocalization.Bind(dialog, ContentDialog.PrimaryButtonTextProperty, "批准一次");
        UiLocalization.Bind(dialog, ContentDialog.CloseButtonTextProperty, "拒绝");
        return dialog;
    }

    private static void AddLabel(StackPanel content, string key, string name = "")
    {
        var label = new TextBlock { Name = name, TextWrapping = TextWrapping.Wrap };
        UiLocalization.Bind(label, TextBlock.TextProperty, key);
        content.Children.Add(label);
    }
}
