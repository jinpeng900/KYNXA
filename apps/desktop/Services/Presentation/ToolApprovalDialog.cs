using System.Text.Json;
using KYNXA.Contracts;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Displays the requested operation; the caller owns approval identity and cancellation.
/// 展示所请求的操作；审批身份与取消仍由调用方负责。
/// </summary>
public static class ToolApprovalDialog
{
    public static ContentDialog Create(XamlRoot root, ToolActivity tool)
    {
        var content = new StackPanel { Spacing = 10 };
        if (tool.Name == "terminal.host.run") AddHostTerminalOperation(content, tool);
        else if (ComputerToolPresentation.Supports(tool.Name)) AddComputerOperation(content, tool);
        else AddStandardOperation(content, tool);
        var dialog = new ContentDialog { XamlRoot = root, Content = new ScrollViewer { Content = content, MaxHeight = 460 },
            DefaultButton = ContentDialogButton.None,
            PrimaryButtonStyle = (Style)Application.Current.Resources["KynxaQuietButtonStyle"],
            CloseButtonStyle = (Style)Application.Current.Resources["KynxaQuietButtonStyle"] };
        UiLocalization.Bind(dialog, ContentDialog.TitleProperty, "批准这次工具操作？");
        UiLocalization.Bind(dialog, ContentDialog.PrimaryButtonTextProperty, "批准一次");
        UiLocalization.Bind(dialog, ContentDialog.CloseButtonTextProperty, "拒绝");
        return dialog;
    }

    private static void AddStandardOperation(StackPanel content, ToolActivity tool)
    {
        content.Children.Add(new TextBlock { Name = "ToolApprovalName", Text = tool.Name, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
        var summary = new TextBlock { Text = tool.Summary, TextWrapping = TextWrapping.Wrap };
        const string SensitiveReadNotice = "读取可能含密钥的文件，批准后内容可能进入工具记录并发送给当前模型。";
        if (tool.Summary == SensitiveReadNotice)
            UiLocalization.Bind(summary, TextBlock.TextProperty, SensitiveReadNotice);
        content.Children.Add(summary);
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
    }

    private static void AddComputerOperation(StackPanel content, ToolActivity tool)
    {
        AddLabel(content, ComputerToolPresentation.ActionKey(tool.Name), "ToolApprovalName");
        AddLabel(content, "本机桌面操作在终端沙箱外执行。", "ToolApprovalDesktopBoundary");
        foreach (var field in ComputerToolPresentation.Fields(tool))
        {
            AddLabel(content, field.LabelKey);
            var value = new TextBlock { TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true };
            if (field.LocalizedValue) UiLocalization.Bind(value, TextBlock.TextProperty, field.Value);
            else value.Text = field.Value;
            content.Children.Add(value);
        }
        string reason = ComputerToolPresentation.Reason(tool);
        if (reason.Length > 0)
        {
            AddLabel(content, "审批原因");
            content.Children.Add(new TextBlock { Name = "ToolApprovalReason", Text = reason, TextWrapping = TextWrapping.Wrap });
        }
    }

    private static void AddHostTerminalOperation(StackPanel content, ToolActivity tool)
    {
        AddLabel(content, "在本机执行命令", "ToolApprovalName");
        bool visible = tool.Arguments is { ValueKind: JsonValueKind.Object } parameters &&
            parameters.TryGetProperty("visible", out var showWindow) && showWindow.ValueKind == JsonValueKind.True;
        var mode = new TextBlock { Name = "ToolApprovalTerminalMode", TextWrapping = TextWrapping.Wrap,
            FontSize = (double)Application.Current.Resources["KynxaCaptionFontSize"],
            Foreground = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["KynxaSecondaryTextBrush"] };
        UiLocalization.Bind(mode, TextBlock.TextProperty, visible ? "显示终端窗口" : "后台执行命令");
        content.Children.Add(mode);
        AddLabel(content, "本机终端在沙箱外执行，可能修改电脑文件和设置。", "ToolApprovalHostBoundary");
        if (tool.Arguments is not { ValueKind: JsonValueKind.Object } args) return;
        foreach (var field in new[] { ("shell", "终端"), ("cwd", "工作范围"), ("script", "命令"), ("reason", "审批原因") })
        {
            if (!args.TryGetProperty(field.Item1, out var value) || value.ValueKind != JsonValueKind.String) continue;
            AddLabel(content, field.Item2);
            content.Children.Add(new TextBlock { Text = value.GetString(), TextWrapping = TextWrapping.Wrap,
                IsTextSelectionEnabled = true, FontSize = (double)Application.Current.Resources["KynxaCaptionFontSize"] });
        }
        if (!args.TryGetProperty("cwd", out _) && tool.WorkspaceRoot is { } workspace)
        {
            AddLabel(content, "工作范围");
            content.Children.Add(new TextBlock { Text = workspace, TextWrapping = TextWrapping.Wrap });
        }
    }

    private static void AddLabel(StackPanel content, string key, string name = "")
    {
        var label = new TextBlock { Name = name, TextWrapping = TextWrapping.Wrap };
        UiLocalization.Bind(label, TextBlock.TextProperty, key);
        content.Children.Add(label);
    }
}
