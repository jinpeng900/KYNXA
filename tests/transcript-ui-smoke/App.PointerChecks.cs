using System.Text.Json;
using KYNXA_Desktop.ViewModels;
using Microsoft.Web.WebView2.Core;
using Windows.Storage.Streams;

namespace TranscriptUiSmoke;

public partial class App
{
    private Windows.Graphics.PointInt32? _pointerWindowPosition;

    // Optional diagnostic: this WebView2/CDP environment delivered trusted mouse events
    // but did not extend selections even in a fresh plain HTML control document. Keep
    // that environmental failure visible instead of claiming physical dragging passed.
    // 可选诊断：此 WebView2/CDP 环境接收到可信鼠标事件，但即使全新纯 HTML 页面也未扩展选择；应明确环境失败，不得声称真实拖拽通过。
    private async Task CheckPointerSelectionAsync()
    {
        var chat = Guid.NewGuid();
        var rows = new List<ConversationMessageViewModel>();
        for (int index = 0; index < 24; index++)
        {
            rows.Add(Message(chat, "user", $"USER_DRAG_{index:00} 用户的问题，可以跨消息选择。"));
            rows.Add(Message(chat, "assistant", $"ASSISTANT_DRAG_{index:00} 回复包含连续可选择的文本。\n\n第二段：将鼠标拖到视口边缘，浏览器应自动滚动。\n\n第三段：松开鼠标后，移动指针不应延长选区。"));
        }
        _transcript.ShowConversation(chat, rows);
        await WaitAsync("document.querySelectorAll('#messages > article.message').length === 48", "pointer fixture renders across many messages");
        _pointerWindowPosition = _window!.AppWindow.Position;
        _window.AppWindow.Move(new Windows.Graphics.PointInt32(-20000, -20000));
        _transcript.Browser.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
        await EvalAsync<bool>("(() => { window.__mouseLog = []; for (const type of ['pointerdown','pointerup','mousedown','mousemove','blur']) window.addEventListener(type, event => __mouseLog.push({type,x:event.clientX,y:event.clientY,buttons:event.buttons,defaultPrevented:event.defaultPrevented,target:event.target.tagName,scroll:scrollY,selection:String(getSelection())}),true); return true; })()");
        foreach (bool upward in new[] { false, true })
        {
            _transcript.ClearSelection();
            await Task.Delay(100);
            var point = await EvalAsync<JsonElement>("__transcriptSmoke.dragPoint('ASSISTANT_DRAG_12')");
            await Task.Delay(120);
            double x = point.GetProperty("x").GetDouble(), y = point.GetProperty("y").GetDouble();
            double edge = upward ? -25 : point.GetProperty("height").GetDouble() + 25;
            double initialTop = await EvalAsync<double>("__transcriptSmoke.scroller().scrollTop");
            await DispatchMouse("mouseMoved", x, y);
            await DispatchMouse("mousePressed", x, y, "left", 1);
            var afterPress = await EvalAsync<JsonElement>("({ranges:getSelection().rangeCount,anchor:getSelection().anchorNode?.textContent,anchorOffset:getSelection().anchorOffset,focus:getSelection().focusNode?.textContent,focusOffset:getSelection().focusOffset,hit:(()=>{let n=document.elementFromPoint(" + x.ToString(System.Globalization.CultureInfo.InvariantCulture) + "," + y.ToString(System.Globalization.CultureInfo.InvariantCulture) + ");const a=[];for(;n;n=n.parentElement)a.push({tag:n.tagName,cls:n.className,select:getComputedStyle(n).userSelect,draggable:n.draggable});return a})()})");
            await DispatchMouse("mouseMoved", x + 90, y, buttons: 1);
            await Task.Delay(70);
            string shortSelection = await EvalAsync<string>("String(getSelection())");
            if (shortSelection.Length == 0)
            {
                await DispatchMouse("mouseReleased", x + 90, y, "left");
                string minimalSelection = await ProbeMinimalPointerSelectionAsync();
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-pointer-minimal.txt"),
                    "Production short drag: " + shortSelection + "\nMinimal page short drag: " + minimalSelection + "\nFocus: " + await EvalAsync<bool>("document.hasFocus()"));
                throw new InvalidOperationException("CDP short drag failed in transcript; minimal plain page selection: " + JsonSerializer.Serialize(minimalSelection));
            }
            for (int step = 1; step <= 8; step++)
            {
                // Pass through the avatar column while dragging; all coordinates stay local to this test WebView.
                // 拖拽时穿过头像列；所有坐标都限制在此测试的 WebView 内。
                await DispatchMouse("mouseMoved", x + (30 - x) * step / 8, y + (edge - y) * step / 8, buttons: 1);
                await Task.Delay(30);
            }
            for (int step = 0; step < 18; step++)
            {
                await DispatchMouse("mouseMoved", 30, edge, buttons: 1);
                await Task.Delay(65);
            }
            double movedTop = await EvalAsync<double>("__transcriptSmoke.scroller().scrollTop");
            await DispatchMouse("mouseReleased", 30, edge, "left");
            await Task.Delay(100);
            string selected = await EvalAsync<string>("window.transcriptSelectionText()");
            File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-mouse.json"), JsonSerializer.Serialize(new { upward, x, y, edge, initialTop, movedTop, afterPress, shortSelection, selected,
                browser = await EvalAsync<JsonElement>("({events:__mouseLog,raw:String(getSelection()),state:transcriptState(),focused:document.hasFocus(),ratio:devicePixelRatio,height:innerHeight})") }, new JsonSerializerOptions { WriteIndented = true }));
            Check(selected.Contains("ASSISTANT_DRAG_") && selected.Contains("USER_DRAG_"), "mouse selection crosses user/assistant messages and avatar lane");
            Check(upward ? movedTop < initialTop - 15 : movedTop > initialTop + 15,
                (upward ? "upward" : "downward") + " browser selection scrolls automatically at viewport edge");
            for (int step = 0; step < 5; step++)
                await DispatchMouse("mouseMoved", 250 + step * 30, 80 + step * 55);
            await Task.Delay(100);
            Check(await EvalAsync<string>("window.transcriptSelectionText()") == selected, "released selection remains unchanged while the pointer moves");
            _metrics[upward ? "upwardSelectionScrollPixels" : "downwardSelectionScrollPixels"] = movedTop - initialTop;
        }
        _transcript.ClearSelection();
    }

    private async Task DispatchMouse(string type, double x, double y, string button = "none", int buttons = 0)
    {
        if (buttons == 1 && button == "none") button = "left";
        await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Input.dispatchMouseEvent", JsonSerializer.Serialize(new
        { type, x, y, button, buttons, clickCount = buttons == 1 || type is "mousePressed" or "mouseReleased" ? 1 : 0, pointerType = "mouse" }));
    }

    private async Task<string> ProbeMinimalPointerSelectionAsync()
    {
        await _transcript.Browser.ExecuteScriptAsync("document.open(); document.write('<!doctype html><html><body><p id=plain style=\"user-select:text;-webkit-user-select:text;margin:40px;font-size:24px\">0123456789 plain selection test abcdefghijklmnopqrstuvwxyz.</p><div style=\"height:3000px\">More text</div></body></html>'); document.close();");
        await Task.Delay(150);
        var point = await EvalAsync<JsonElement>("(() => { const text=document.querySelector('#plain').firstChild; const r=document.createRange();r.setStart(text,3);r.setEnd(text,4);const p=r.getBoundingClientRect();return{x:p.x+p.width/2,y:p.y+p.height/2};})()");
        double x = point.GetProperty("x").GetDouble(), y = point.GetProperty("y").GetDouble();
        await DispatchMouse("mouseMoved", x, y);
        await DispatchMouse("mousePressed", x, y, "left", 1);
        for (int i = 1; i <= 10; i++)
        {
            await DispatchMouse("mouseMoved", x + i * 10, y, "left", 1);
            await Task.Delay(30);
        }
        await DispatchMouse("mouseReleased", x + 100, y, "left");
        return await EvalAsync<string>("String(getSelection())");
    }

    private async Task CaptureVisualPreviewAsync()
    {
        var chat = Guid.NewGuid();
        var rows = new[]
        {
            Message(chat, "user", "用公式、表格和代码整理几个常见物理量。"),
            Message(chat, "assistant", """
                ## 公式与文字

                行内分数 $v=\frac{s}{t}$ 与根号 $\sqrt{x^2+y^2}$ 保持清晰。English: $E=mc^2$.

                $$
                \int_0^1 x^2\,dx=\frac13
                $$

                | 名称 | 公式 | 说明 |
                | --- | --- | --- |
                | 角频率 | $\omega=2\pi f$ | 周期性运动 |
                | 感应电动势 | $\varepsilon=-\frac{d\Phi_B}{dt}$ | 磁通量的变化率 |
                | 理想气体 | $\frac{pV}{T}=\text{常量}$ | 固定质量 |

                ```python
                def speed(distance, seconds):
                    return distance / seconds
                ```
                """)
        };
        _transcript.ShowConversation(chat, rows);
        await WaitAsync("document.querySelectorAll('.katex').length === 7 && document.fonts.status === 'loaded'", "visual DOM fixture is fully typeset");
        await Task.Delay(150);
        await EvalAsync<bool>("(() => { scrollTo(0,0); return true; })()");
        await Task.Delay(100);
        using var capture = new InMemoryRandomAccessStream();
        await _transcript.Browser.CoreWebView2.CapturePreviewAsync(CoreWebView2CapturePreviewImageFormat.Png, capture);
        capture.Seek(0);
        string path = Path.Combine(Path.GetTempPath(), "kynxa-transcript-preview.png");
        await using var file = File.Create(path);
        await capture.AsStreamForRead().CopyToAsync(file);
        _metrics["previewPath"] = path;
    }
}
