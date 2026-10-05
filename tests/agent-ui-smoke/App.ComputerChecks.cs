using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace AgentUiSmoke;

public partial class App
{
    private async Task CheckComputerToolsAsync()
    {
        await WaitAsync(() => _root.XamlRoot is not null && _api.Reads == 1 && Button("AgentRefreshButton").IsEnabled,
            "isolated window is ready for computer approval and result dialogs");
        await CheckHostTerminalApprovalAsync();
        const string typed = "PRIVATE_INPUT_FIXTURE_中文";
        var input = new ToolActivity("computer-type", "computer.type", JsonSerializer.SerializeToElement(new
        {
            windowId = "123456", processId = 789, text = typed, reason = "Enter " + typed + " into the selected editor"
        }), "approval-required", typed, ApprovalId: Guid.NewGuid(), Sandbox: "host-desktop");
        string original = input.Arguments!.Value.GetRawText();
        var dialog = ToolApprovalDialog.Create(_root.XamlRoot, input);
        var decision = dialog.ShowAsync();
        await WaitAsync(() => NativeUi.OpenDialog(_root) == dialog, "computer input approval opens as a native dialog");
        await SettleAsync();
        string text = DialogText(dialog);
        Check(NativeUi.ByName<TextBlock>(dialog, "ToolApprovalName").Text == "输入文字", "input approval uses a human action label");
        Check(text.Contains("HWND 123456 · PID 789") && text.Contains(typed.Length.ToString()), "input approval shows exact target and character count");
        Check(!text.Contains(typed) && !NativeUi.Descendants<TextBox>(dialog).Any(), "input approval never renders the private input or JSON parameters");
        Check(NativeUi.ByName<TextBlock>(dialog, "ToolApprovalDesktopBoundary").Text.Contains("沙箱外"), "approval states the actual host desktop boundary");
        Check(dialog.DefaultButton == ContentDialogButton.None && input.Arguments.Value.GetRawText() == original,
            "display does not approve automatically or modify the original arguments");
        UiText.Initialize("en");
        await SettleAsync();
        Check(NativeUi.ByName<TextBlock>(dialog, "ToolApprovalName").Text == "Type text" &&
            NativeUi.ByName<TextBlock>(dialog, "ToolApprovalDesktopBoundary").Text.Contains("outside the terminal sandbox") &&
            DialogText(dialog).Contains("Character count") && dialog.PrimaryButtonText == "Approve once",
            "computer action, scope, fields and approval buttons switch to English live");
        Check(!DialogText(dialog).Contains(typed) && input.Arguments.Value.GetRawText() == original, "language switching preserves input privacy and original request");
        await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "computer-input-approval-en.png"));
        NativeUi.InvokeDialogButton(dialog, primary: false);
        Check(await decision == ContentDialogResult.None, "explicit denial cannot approve desktop input");
        await Task.Delay(250);

        UiText.Initialize("zh-CN");
        var launch = new ToolActivity("computer-launch", "computer.launch", JsonSerializer.SerializeToElement(new
        {
            appPath = Path.Combine(_directory, "Example Editor.exe"), args = new[] { "a file.txt", "--literal=<fixture>" }, reason = "Open the requested local editor"
        }), "approval-required", "Open app", ApprovalId: Guid.NewGuid(), Sandbox: "host-desktop");
        dialog = ToolApprovalDialog.Create(_root.XamlRoot, launch);
        decision = dialog.ShowAsync();
        await WaitAsync(() => NativeUi.OpenDialog(_root) == dialog, "computer launch approval opens without executing an application");
        text = DialogText(dialog);
        Check(text.Contains("打开软件") && text.Contains("Example Editor.exe") && text.Contains("\"a file.txt\"") && text.Contains("--literal=<fixture>"),
            "launch approval displays the exact application and literal command arguments");
        Check(!NativeUi.Descendants<TextBox>(dialog).Any() && !text.Contains("HWND"), "launch has human fields and no fabricated window target");
        NativeUi.InvokeDialogButton(dialog, primary: true);
        Check(await decision == ContentDialogResult.Primary, "only an explicit primary action approves the local application request");
        await Task.Delay(250);

        foreach (string name in new[] { "computer.windows", "computer.apps" })
        {
            var discovery = input with { Name = name, Arguments = JsonSerializer.SerializeToElement(new { reason = "Discover available desktop targets" }) };
            Check(ComputerToolPresentation.Supports(name) && ComputerToolPresentation.Fields(discovery).Count == 0,
                name + " has no invented target or input fields");
        }
        var drag = input with { Name = "computer.drag", Arguments = JsonSerializer.SerializeToElement(new
            { windowId = "123456", processId = 789, x = 12, y = 34, endX = 56, endY = 78, reason = "Move a selected item" }) };
        Check(ComputerToolPresentation.Fields(drag).Any(field => field.Value == "(12, 34) → (56, 78)"), "drag approval identifies exact start and end coordinates");
        var scroll = input with { Name = "computer.scroll", Arguments = JsonSerializer.SerializeToElement(new
            { windowId = "123456", processId = 789, x = 12, y = 34, delta = -120, reason = "Read the next section" }) };
        Check(ComputerToolPresentation.Fields(scroll).Any(field => field.LabelKey == "滚动量" && field.Value == "-120"), "scroll approval preserves direction and amount");
        var key = input with { Name = "computer.key", Arguments = JsonSerializer.SerializeToElement(new
            { windowId = "123456", processId = 789, key = "CTRL+A", reason = "Select the editor content" }) };
        Check(ComputerToolPresentation.Fields(key).Any(field => field.Value == "CTRL+A"), "key approval preserves the exact shortcut");

        // Capture only this test's own fake-data window; no user desktop, model or MCP is read.
        // 仅捕获本测试自有的虚构数据窗口，不读取用户桌面、模型或 MCP。
        _api.ScreenshotImagePath = Path.Combine(_directory, "isolated-screenshot-source.png");
        await NativeWindowCapture.CaptureAsync(_window!, _api.ScreenshotImagePath);
        var screenshot = input with { Name = "computer.screenshot", Status = "completed", ResultRef = _api.ResultReference,
            Arguments = JsonSerializer.SerializeToElement(new { windowId = "123456", processId = 789, reason = "Show the isolated captured window" }) };
        using (var preview = new ToolResultDialog(_root.XamlRoot, _api, Guid.NewGuid(), screenshot))
        {
            Check(_api.ResultReads == 0 && _api.ResultPages == 0, "constructing a screenshot viewer performs no implicit result fetch");
            var shown = preview.ShowAsync();
            await WaitAsync(() => NativeUi.OpenDialog(_root) == preview.Dialog && NativeUi.Descendants<Image>(preview.Dialog).Any(image => image.Source is not null),
                "explicit screenshot viewer loads the archived PNG into a native image");
            Check(_api.ResultReads == 1 && _api.ResultPages == 0, "screenshot uses the existing typed media API without JSON pagination");
            Check(preview.Dialog.Title?.ToString() == "截图预览" && !NativeUi.Descendants<TextBox>(preview.Dialog).Any(),
                "screenshot viewer renders an image and concise title without base64 or JSON text");
            await NativeWindowCapture.CaptureAsync(_window!, Path.Combine(_directory, "computer-screenshot-preview-zh.png"));
            UiText.Initialize("en");
            await SettleAsync();
            Check(preview.Dialog.Title?.ToString() == "Screenshot preview" &&
                NativeUi.ByName<Button>(preview.Dialog, "ToolResultMedia").Content?.ToString() == "View screenshot",
                "screenshot preview labels switch language without another fetch");
            Check(_api.ResultReads == 1, "language switching does not reload screenshot data");
            preview.Dialog.Hide();
            await shown;
        }
        Check(_api.Saves == 0 && _api.Connections == 0 && _api.PreviewReads == 0,
            "desktop presentation tests never execute commands, connect MCP, save config or invoke models");
        UiText.Initialize("zh-CN");
    }

    private async Task CheckHostTerminalApprovalAsync()
    {
        // Verify consent text without reading a credential file or executing a tool.
        // 不读取凭据文件或执行工具，验证审批明确告知模型传输范围并支持即时切换语言。
        var sensitive = new ToolActivity("sensitive-read-fixture", "filesystem.read",
            JsonSerializer.SerializeToElement(new { path = ".env" }), "approval-required",
            "读取可能含密钥的文件，批准后内容可能进入工具记录并发送给当前模型。",
            ApprovalId: Guid.NewGuid(), WorkspaceRoot: _directory);
        var sensitiveDialog = ToolApprovalDialog.Create(_root.XamlRoot, sensitive);
        var sensitiveDecision = sensitiveDialog.ShowAsync();
        await WaitAsync(() => NativeUi.OpenDialog(_root) == sensitiveDialog, "sensitive read approval opens without reading credentials");
        Check(NativeUi.Descendants<TextBlock>(sensitiveDialog).Any(block => block.Text.Contains("发送给当前模型")),
            "sensitive read approval states model transmission in Chinese");
        UiText.Initialize("en");
        await SettleAsync();
        Check(NativeUi.Descendants<TextBlock>(sensitiveDialog).Any(block => block.Text.Contains("sent to the current model")),
            "sensitive read consent switches live to English");
        sensitiveDialog.Hide();
        await sensitiveDecision;
        UiText.Initialize("zh-CN");

        foreach (bool visible in new[] { false, true })
        {
            var activity = new ToolActivity("host-terminal-fixture", "terminal.host.run", JsonSerializer.SerializeToElement(new
            {
                shell = "cmd", script = "echo KYNXA visible fixture", visible, reason = "Run a synthetic demo"
            }), "approval-required", "Command", ApprovalId: Guid.NewGuid(), WorkspaceRoot: _directory);
            string original = activity.Arguments!.Value.GetRawText();
            var dialog = ToolApprovalDialog.Create(_root.XamlRoot, activity);
            var decision = dialog.ShowAsync();
            await WaitAsync(() => NativeUi.OpenDialog(_root) == dialog, "host command approval opens without running a terminal");
            await SettleAsync();
            Check(NativeUi.ByName<TextBlock>(dialog, "ToolApprovalTerminalMode").Text == (visible ? "显示终端窗口" : "后台执行命令"),
                "host approval identifies requested visible or background execution");
            Check(DialogText(dialog).Contains("echo KYNXA visible fixture") && DialogText(dialog).Contains(_directory) &&
                !NativeUi.Descendants<TextBox>(dialog).Any(), "host approval shows literal command and effective scope without JSON");
            UiText.Initialize("en");
            await SettleAsync();
            Check(NativeUi.ByName<TextBlock>(dialog, "ToolApprovalTerminalMode").Text == (visible ? "Show terminal window" : "Run command in background") &&
                dialog.PrimaryButtonText == "Approve once", "host execution mode and actions change language live");
            Check(activity.Arguments.Value.GetRawText() == original && dialog.DefaultButton == ContentDialogButton.None,
                "host approval preserves command and visibility without automatic execution");
            NativeUi.InvokeDialogButton(dialog, primary: false);
            Check(await decision == ContentDialogResult.None, "denying the host command cannot execute it");
            await Task.Delay(250);
            UiText.Initialize("zh-CN");
        }
    }

    private static string DialogText(ContentDialog dialog) => string.Join("\n", NativeUi.Descendants<TextBlock>(dialog).Select(block => block.Text));
}
