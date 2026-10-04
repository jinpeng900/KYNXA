using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using Windows.ApplicationModel.DataTransfer;

namespace TranscriptUiSmoke;

public partial class App
{
    private async Task CheckToolActivitiesAsync()
    {
        string language = UiText.Language;
        try
        {
            UiText.Initialize("zh-CN");
            var chat = Guid.NewGuid();
            var message = Message(chat, "assistant", "工具活动测试。以下内容来自隔离样例，不执行命令。", "streaming");
            var resultId = Guid.NewGuid();
            var approvalId = Guid.NewGuid();
            message.Message.ToolActivities =
            [
                new("terminal-fixture", "terminal.run", JsonSerializer.SerializeToElement(new { command = "node", args = new[] { "scripts/check.mjs", "sample file.txt" } }), "completed", "Fixture terminal request", "Fixture result\n  preserved indent"),
                new("skill-fixture", "skill.run", JsonSerializer.SerializeToElement(new { id = "fixture-skill", path = "scripts/verify.mjs", args = new[] { "sample.txt" } }), "running", "Fixture skill request"),
                new("mcp-fixture", "mcp.fixture.custom", JsonSerializer.SerializeToElement(new { arguments = new { url = "https://example.invalid", reason = 42 }, policy = new { reason = "Isolated fixture" } }), "approval-required", "Fixture approval", ApprovalId: approvalId, OutsideWorkspace: false, Sandbox: "trusted-external-process", WorkspaceRoot: "fixture-workspace", ResultRef: new(resultId, 1048576, new string('a', 64))),
                new("error-fixture", "mcp.fixture.operation", JsonSerializer.SerializeToElement(new { opaque = 123 }), "unknown", "Fixture interrupted result", Code: "MCP_REQUEST_CANCELLED")
            ];
            _transcript.ShowConversation(chat, [message]);
            await WaitAsync("document.querySelectorAll('.tool-activity').length === 4", "real DTO renders four tool activities");
            Check(await EvalAsync<bool>("[...document.querySelectorAll('.tool-activity')].every(node => node.tagName === 'DIV') && !document.querySelector('.tool-activities summary,.tool-details')"), "tool progress is direct compact text without disclosure cards");
            await EvalAsync<bool>("(() => { scrollTo(0,0); return true; })()");
            await Task.Delay(100);
            _metrics["toolSummaryPreview"] = Path.Combine(Path.GetTempPath(), "kynxa-transcript-tools-summary.png");
            await CaptureViewportAsync((string)_metrics["toolSummaryPreview"]);
            Check(await EvalAsync<bool>("!document.querySelector('.tool-arguments,.tool-result,.tool-metadata,.tool-view-result,.tool-detail-label,.tool-details button')"), "compact activities never create argument JSON, receipts, metadata or result viewer buttons");
            Check(await EvalAsync<bool>("[...document.querySelectorAll('.tool-title')].map(node => node.textContent).join(',') === '运行命令,使用技能,执行操作,执行操作' && [...document.querySelectorAll('.tool-title')].every(node => getComputedStyle(node).fontSize === '12px' && getComputedStyle(node).color === 'rgb(115, 115, 115)')"), "real tool names become small gray human actions rather than MCP identifiers");
            Check(await EvalAsync<bool>("document.querySelector('[data-tool-call-id=terminal-fixture] .tool-command').textContent === 'node scripts/check.mjs \"sample file.txt\"' && document.querySelector('[data-tool-call-id=skill-fixture] .tool-command').textContent === 'node scripts/verify.mjs sample.txt' && document.querySelector('[data-tool-call-id=mcp-fixture] .tool-command').textContent === 'https://example.invalid'"), "headers display commands and business action from real parameters");
            Check(await EvalAsync<bool>("document.querySelector('[data-tool-call-id=error-fixture] .tool-command').hidden && !document.querySelector('[data-tool-call-id=error-fixture]').textContent.includes('MCP_REQUEST_CANCELLED') && document.querySelector('[data-tool-call-id=error-fixture] .tool-error').textContent === '操作已中断，执行结果尚未确定。'"), "unknown outcome is honest and localized without exposing technical error code");

            Check(await EvalAsync<bool>("KynxaToolPresentation.describe({name:'mcp.fixture.custom',status:'error',result:JSON.stringify({debug:{message:'private'},_meta:{message:'hidden'}})}).errorText === '' && KynxaToolPresentation.describe({name:'mcp.fixture.custom',arguments:{id:'opaque'},status:'running'}).action === ''"), "unknown JSON failure and opaque identity are not presented as readable actions");
            Check(await EvalAsync<bool>("KynxaToolPresentation.describe({name:'filesystem.read',status:'error',result:JSON.stringify({message:'File unavailable.'})}).errorText === 'File unavailable.' && KynxaToolPresentation.describe({name:'filesystem.read',status:'completed',result:'Completed raw receipt'}).errorText === ''"), "only actual public errors become readable failure detail");

            await EvalAsync<bool>("(() => { window.__toolItem = document.querySelector('[data-tool-call-id=mcp-fixture]'); window.__toolBody = document.querySelector('.message-body').firstChild; return true; })()");
            UiText.Initialize("en");
            await WaitAsync("document.querySelector('[data-tool-call-id=mcp-fixture] .tool-state').textContent === 'Awaiting approval' && document.querySelector('[data-tool-call-id=mcp-fixture] .tool-title').textContent === 'Perform action' && document.querySelector('[data-tool-call-id=terminal-fixture] .tool-title').textContent === 'Run command' && document.querySelector('[data-tool-call-id=error-fixture] .tool-error').textContent.includes('outcome is not yet confirmed')", "friendly actions, status and failure detail switch language immediately");
            Check(await EvalAsync<bool>("document.querySelector('[data-tool-call-id=mcp-fixture]') === window.__toolItem && document.querySelector('.message-body').firstChild === window.__toolBody"), "language change preserves compact action and original reply DOM");
            UiText.Initialize("zh-CN");
            await WaitAsync("document.querySelector('[data-tool-call-id=mcp-fixture] .tool-state').textContent === '等待批准'", "tool language restores without resetting details");

            message.Message.ToolActivities.AddRange(Enumerable.Range(0, 200).Select(index => new ToolActivity("batch-" + index, "filesystem.read", JsonSerializer.SerializeToElement(new { path = "sample-" + index + ".txt" }), index == 199 ? "running" : "completed", "Fixture read")));
            message.Refresh();
            await WaitAsync("document.querySelector('[data-tool-call-id=batch-199]') !== null && document.querySelectorAll('.tool-activity').length < 12", "204 source activities project a bounded recent window");
            Check(message.Message.ToolActivities.Count == 204, "bounded UI preserves all 204 formal source activities");
            Check(await EvalAsync<bool>("document.querySelector('[data-tool-call-id=mcp-fixture] .tool-state').dataset.status === 'approval-required' && document.querySelector('[data-tool-call-id=mcp-fixture]').checkVisibility() && !document.querySelector('[data-tool-call-id=batch-0]')"), "old pending approval stays visible while obsolete ordinary actions leave DOM");
            await EvalAsync<bool>("(() => { window.__unchangedTool = document.querySelector('[data-tool-call-id=mcp-fixture]'); window.__toolMutations = 0; window.__toolObserver = new MutationObserver(records => window.__toolMutations += records.length); window.__toolObserver.observe(window.__unchangedTool,{attributes:true,childList:true,characterData:true,subtree:true}); return true; })()");
            // Let the disclosure toggle finish before checking stream-driven mutations.
            // 先等待折叠控件切换完成，再检查流式驱动的变化。
            await Task.Delay(50);
            await EvalAsync<bool>("(() => { window.__toolMutations = 0; return true; })()");
            message.Message.ToolActivities[^1] = message.Message.ToolActivities[^1] with { Status = "completed", Result = "Fixture done" };
            message.Message.Content += "\n\n新的正文片段。";
            message.Refresh();
            await WaitAsync("[...document.querySelectorAll('.tool-activity')].some(row=>row.querySelector('.tool-command')?.textContent.includes('sample-199.txt') && row.querySelector('.tool-state')?.dataset.status === 'completed') && document.querySelector('.message-body').textContent.includes('新的正文片段')", "changed tool and stream content update together");
            Check(await EvalAsync<bool>("window.__toolMutations === 0 && window.__unchangedTool === document.querySelector('[data-tool-call-id=mcp-fixture]') && window.__toolBody === document.querySelector('.message-body').firstChild"), "bounded update preserves unrelated approval and stable paragraph DOM");
            await EvalAsync<bool>("(() => { window.__toolObserver.disconnect(); return true; })()");
            _metrics["toolUnchangedItemMutations"] = 0;

            _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(480, 850));
            await Task.Delay(120);
            Check(await EvalAsync<bool>("document.documentElement.scrollWidth <= innerWidth && [...document.querySelectorAll('.tool-activity')].every(node => node.getBoundingClientRect().right <= innerWidth)"), "friendly tool headers and actions adapt to narrow chat without clipping");
            _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 850));
        }
        finally { UiText.Initialize(language); _transcript.ClearSelection(); }
    }

}
