using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Views;
using Microsoft.UI.Xaml.Controls;

namespace ScreenshotPanelUiSmoke;

public partial class App
{
    private async Task CheckShellScreenshotRoutingAsync()
    {
        var conversation = Guid.NewGuid(); var api = new FakeScreenshotApi();
        var tool = Screenshot("shell-browser") with { Name = "mcp.custom-browser.browser_take_screenshot" };
        var row = Message(conversation, tool);
        var shell = new ShellPage(api, conversation, [row]);
        Grid.SetColumn(shell, 1); _layout.Children.Add(shell);
        try
        {
            await WaitAsync(() => shell.XamlRoot is not null, "production Shell result-routing partial has a live fixture XamlRoot");
            shell.OpenScreenshot(new(Guid.NewGuid(), row.Message.Id, tool));
            shell.OpenScreenshot(new(conversation, Guid.NewGuid(), tool));
            Check(api.Reads.Count == 0, "Shell refuses screenshot requests belonging to another chat or message");
            shell.OpenScreenshot(new(conversation, row.Message.Id, tool));
            await WaitAsync(() => api.Reads.Count == 1, "production Shell routing opens a custom-named MCP screenshot instead of filtering it as non-native");
            var closed = shell.ResultClosed!;
            shell.OpenScreenshot(new(conversation, row.Message.Id, tool));
            Check(api.Reads.Count == 1 && api.Reads[0].Conversation == conversation && api.Reads[0].Reference == tool.ResultRef,
                "Shell uses the current formal receipt and prevents duplicate screenshot windows");
            shell.ChangeConversation(Guid.NewGuid());
            await WaitAsync(() => closed.IsCompleted, "Shell conversation-change event closes its independent screenshot viewer");
            Check(api.Reads[0].Token.IsCancellationRequested, "Shell cancellation reaches the original archive request");
        }
        finally { shell.ScreenshotPanel.Dispose(); _layout.Children.Remove(shell); _window.Activate(); }
    }
}
