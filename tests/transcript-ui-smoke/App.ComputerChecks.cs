using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.ViewModels;
using Windows.ApplicationModel.DataTransfer;

namespace TranscriptUiSmoke;

public partial class App
{
    private async Task CheckComputerToolsAsync()
    {
        string language = UiText.Language;
        const string privateInput = "PRIVATE_NATIVE_INPUT_中文";
        var chat = Guid.NewGuid();
        var reply = Message(chat, "assistant", "DESKTOP_PROGRESS\n\nReading the requested **local window**.", "streaming");
        var firstResult = new ToolResultReference(Guid.NewGuid(), 20000, new string('a', 64));
        var secondResult = new ToolResultReference(Guid.NewGuid(), 20000, new string('b', 64));
        reply.Message.ToolActivities =
        [
            new("window-read", "computer.read", JsonSerializer.SerializeToElement(new { windowId = "123456", processId = 789, reason = "Read a selected target" }), "completed", "Read local window"),
            new("desktop-type", "computer.type", JsonSerializer.SerializeToElement(new { windowId = "123456", processId = 789, text = privateInput, reason = "Enter " + privateInput }), "approval-required", privateInput, ApprovalId: Guid.NewGuid()),
            new("desktop-launch", "computer.launch", JsonSerializer.SerializeToElement(new { appPath = "C:\\Fixture Editor\\editor.exe", args = new[] { "a file.txt", "--literal=<fixture>" }, reason = "Open editor" }), "running", "Open app"),
            new("desktop-scroll", "computer.scroll", JsonSerializer.SerializeToElement(new { windowId = "123456", processId = 789, x = 12, y = 34, delta = -120, reason = "Read next section" }), "completed", "Scroll local target"),
            new("screenshot-first", "computer.screenshot", JsonSerializer.SerializeToElement(new { windowId = "123456", processId = 789, reason = "Capture selected target" }), "completed", "Captured target", ResultRef: firstResult),
            new("screenshot-second", "computer.screenshot", JsonSerializer.SerializeToElement(new { windowId = "123456", processId = 789, reason = "Capture latest target" }), "completed", "Captured target", ResultRef: secondResult)
        ];
        ToolResultRequest? requested = null;
        int requests = 0;
        EventHandler<ToolResultRequest> receive = (_, value) => { requested = value; requests++; };
        _transcript.ToolResultRequested += receive;
        try
        {
            UiText.Initialize("zh-CN");
            _transcript.ShowConversation(chat, [reply]);
            await WaitAsync("document.querySelectorAll('.tool-activity').length === 5 && document.querySelector('[data-tool-call-id=screenshot-first]').dataset.toolCount === '2' && !document.querySelector('.tool-preview,.screenshot-result,.message-attachments')", "actual desktop DTOs merge consecutive screenshots without chat preview controls");
            Check(await EvalAsync<bool>("[...document.querySelectorAll('.tool-title')].map(node=>node.textContent).join(',') === '读取窗口,输入文字,打开软件,滚动,截图 ×2'"), "desktop tools use human action labels and merge counts rather than internal identifiers");
            Check(await EvalAsync<bool>($"!document.getElementById('messages').textContent.includes({JsonSerializer.Serialize(privateInput)}) && ![...document.querySelectorAll('[title]')].some(node=>node.title.includes({JsonSerializer.Serialize(privateInput)}))"), "desktop input is absent from rendered text and tooltips");
            Check(await EvalAsync<bool>($"document.querySelector('[data-tool-call-id=desktop-type] .tool-command').textContent === 'HWND 123456 · PID 789 · {privateInput.Length}字符' && document.querySelector('[data-tool-call-id=desktop-type] .tool-state').dataset.status === 'approval-required'"), "input displays real target and character count with waiting approval state");
            Check(await EvalAsync<bool>("document.querySelector('[data-tool-call-id=desktop-launch] .tool-command').textContent.includes('C:\\\\Fixture Editor\\\\editor.exe') && document.querySelector('[data-tool-call-id=desktop-launch] .tool-command').textContent.includes('a file.txt') && document.querySelector('[data-tool-call-id=desktop-launch] .tool-command').textContent.includes('--literal=<fixture>')"), "launch action retains actual application and literal arguments without HTML execution");
            Check(await EvalAsync<bool>("document.querySelector('[data-tool-call-id=desktop-scroll] .tool-command').textContent === 'HWND 123456 · PID 789 · (12, 34) · 滚动量 -120'"), "scroll retains target, coordinates and exact signed delta");
            Check(await EvalAsync<bool>("[...document.querySelectorAll('.tool-title,.tool-command')].filter(node=>!node.hidden).every(node=>getComputedStyle(node).fontSize==='12px' && getComputedStyle(node).color==='rgb(115, 115, 115)') && !document.querySelector('.tool-arguments,.tool-result,.tool-details,.tool-metadata,.tool-preview,.screenshot-result')"), "desktop actions stay small gray without JSON or screenshot controls");
            await EvalAsync<bool>($"(() => {{ chrome.webview.postMessage({JsonSerializer.Serialize(new { type = "toolResult", conversationId = chat, messageId = reply.Message.Id, toolCallId = "screenshot-second", resultId = secondResult.Id })}); return true; }})()");
            await Task.Delay(100);
            Check(requests == 1 && requested?.ConversationId == chat && requested.MessageId == reply.Message.Id &&
                requested.Tool.ToolCallId == "screenshot-second" && requested.Tool.ResultRef?.Id == secondResult.Id,
                "generic result bridge still validates conversation, message, call and result identity");
            await EvalAsync<bool>($"(() => {{ chrome.webview.postMessage({JsonSerializer.Serialize(new { type = "toolResult", conversationId = chat, messageId = reply.Message.Id, toolCallId = "screenshot-second", resultId = Guid.NewGuid() })}); return true; }})()");
            await Task.Delay(100);
            Check(requests == 1, "a forged screenshot result identity cannot open the native viewer");
            await EvalAsync<bool>("(() => { window.__desktopBody=document.querySelector('.message-body').firstChild; window.__desktopTool=document.querySelector('[data-tool-call-id=desktop-type]'); return true; })()");
            UiText.Initialize("en");
            await WaitAsync($"document.querySelector('[data-tool-call-id=desktop-type] .tool-title').textContent === 'Type text' && document.querySelector('[data-tool-call-id=desktop-type] .tool-command').textContent.includes('{privateInput.Length} characters') && document.querySelector('[data-tool-call-id=desktop-scroll] .tool-command').textContent.includes('Scroll delta -120') && !document.querySelector('.tool-preview,.screenshot-result')", "computer actions and count formats switch language without adding screenshot controls");
            Check(await EvalAsync<bool>("window.__desktopBody === document.querySelector('.message-body').firstChild && window.__desktopTool === document.querySelector('[data-tool-call-id=desktop-type]')"), "live localization preserves existing reply and target action DOM");
            await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), "kynxa-transcript-computer-tools.png"));
            reply.Message.ToolActivities[^1] = reply.Message.ToolActivities[^1] with { Status = "running", ResultRef = null };
            reply.Refresh();
            await WaitAsync("document.querySelector('[data-tool-call-id=screenshot-second] .tool-state')?.dataset.status === 'running' && !document.querySelector('.tool-preview,.screenshot-result')", "running and completed screenshot events never add chat preview controls");
            reply.Message.AssistantSegments =
            [new("desktop-progress", 1, 0, "commentary", "completed", reply.Message.Content, string.Empty),
             new("desktop-final", 2, 1, "final_answer", "completed", "FINAL_DESKTOP_VERIFIED\n\n**Done.**", string.Empty)];
            reply.Message.Content = reply.Message.AssistantSegments[^1].Content;
            reply.Refresh();
            await WaitAsync("document.querySelectorAll('.assistant-segment').length === 2 && document.querySelector('.tool-activity') !== null && !document.querySelector('.final-answer')", "a decoded final phase does not remove desktop actions before the request completes");
            reply.Message.Status = "completed";
            reply.Message.DurationMs = 37000;
            reply.Refresh();
            await WaitAsync("document.querySelector('.final-answer .message-body strong') !== null && !document.querySelector('.tool-activity,.tool-preview,.screenshot-result,.message-attachments') && document.querySelector('.message-elapsed').textContent.includes('37')", "successful final render removes all desktop process and screenshot controls");
            Check(reply.Message.ToolActivities.Count == 6 && reply.Message.ToolActivities[^2].ResultRef == firstResult,
                "final-only rendering preserves archived screenshot references in formal records");
            Check(requests == 1 && await EvalAsync<bool>("!document.querySelector('#messages img:not(.message-avatar),#messages canvas')"), "chat completion never automatically reads or displays screenshot payloads");
            await EvalAsync<bool>("(() => { document.querySelector('.assistant .message-actions > .copy-message').click(); return true; })()");
            await Task.Delay(100);
            Check(await Clipboard.GetContent().GetTextAsync() == reply.Message.Content, "final clipboard contains only the rendered final Markdown");
            string selected = await EvalAsync<string>("(() => { const range=document.createRange();range.selectNodeContents(document.querySelector('.assistant'));getSelection().removeAllRanges();getSelection().addRange(range);return window.transcriptSelectionText(); })()");
            Check(selected.Contains("FINAL_DESKTOP_VERIFIED") && !selected.Contains("View screenshot") && !selected.Contains("37"), "cross-message selection copying includes only final prose");
            _transcript.ClearSelection();
            await EvalAsync<bool>("(() => { window.__finalDesktopBody=document.querySelector('.final-answer .message-body').firstChild; return true; })()");
            UiText.Initialize("zh-CN");
            await WaitAsync("document.querySelector('.message-elapsed').textContent === '用时 37秒' && window.__finalDesktopBody===document.querySelector('.final-answer .message-body').firstChild && !document.querySelector('.tool-preview,.screenshot-result')", "final language switching preserves prose without adding screenshot buttons");
            var restored = new ConversationMessageViewModel(chat, JsonSerializer.Deserialize<ChatMessageState>(JsonSerializer.Serialize(reply.Message))!);
            _transcript.ShowConversation(Guid.NewGuid(), []);
            _transcript.ShowConversation(chat, [restored]);
            await WaitAsync("document.querySelector('.final-answer .message-body strong') !== null && !document.querySelector('.tool-activity,.tool-preview,.screenshot-result,.message-attachments')", "reopening serialized final history preserves final-only chat without screenshot controls");
            await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), "kynxa-transcript-final-screenshot.png"));
            await EvalAsync<bool>($"(() => {{ chrome.webview.postMessage({JsonSerializer.Serialize(new { type = "toolResult", conversationId = chat, messageId = restored.Message.Id, toolCallId = "screenshot-first", resultId = firstResult.Id })}); return true; }})()");
            await Task.Delay(100);
            Check(requests == 2 && requested?.ConversationId == chat && requested.MessageId == restored.Message.Id && requested.Tool.ToolCallId == "screenshot-first"
                && requested.Tool.ResultRef?.Id == firstResult.Id, "generic result bridge can revalidate the formal screenshot record after final history reload");
        }
        finally
        {
            _transcript.ToolResultRequested -= receive;
            UiText.Initialize(language);
            _transcript.ClearSelection();
        }
    }
}
