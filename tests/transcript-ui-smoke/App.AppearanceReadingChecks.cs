using System.Text.Json;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using Windows.ApplicationModel.DataTransfer;
using Windows.UI;
using Windows.UI.ViewManagement;

namespace TranscriptUiSmoke;

public partial class App
{
    // Exercise live preferences on the production control; never modify OS appearance or formal chat data.
    // 在正式控件上验证实时偏好，不修改系统外观或正式聊天数据。
    private async Task CheckAppearanceReadingAsync()
    {
        string paletteId = AppearanceService.Current.Id, mode = AppearanceService.Mode;
        string? customAccent = AppearanceService.CustomAccent;
        double fontSize = AppearanceService.ChatFontSize;
        var size = _window!.AppWindow.Size;
        var chat = Guid.NewGuid();
        string source = string.Join("\n", Enumerable.Range(1, 40).Select(index => $"{index:000} THEME_LINE 中文 English " + new string('x', 120)));
        string content = string.Join("\n\n", Enumerable.Range(1, 24).Select(index => $"THEME_BEFORE_{index:000} 中文 English 正文段落。"))
            + "\n\nTHEME_ANCHOR 中文 English\n\n```txt\n" + source + "\n```"
            + "\n\n```python\n# 中文注释\ndef example(value):\n    if value > 42:\n        return \"中文字符串 English\"\n```"
            + "\n\n> 引用文字中文 English\n\n`inline code` 与 $E=mc^2$。\n\n$$a^2+b^2=c^2$$"
            + "\n\n| 中文名称 | English | 数字 | 说明 |\n| --- | --- | --- | --- |\n| 表格内容 | TableToken | 2026 | 自适应内容 |"
            + "\n\n" + MermaidFence("flowchart LR\n  A[中文 Start] --> B[English 完成]")
            + "\n\n" + string.Join("\n\n", Enumerable.Range(1, 65).Select(index => $"THEME_AFTER_{index:000} 保留阅读位置与正文。"));
        var answer = Message(chat, "assistant", content);
        string formal = JsonSerializer.Serialize(answer.Message);
        Exception? validationError = null;
        try
        {
            await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Emulation.setFocusEmulationEnabled", "{\"enabled\":true}");
            _transcript.Browser.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
            AppearanceService.ApplyPreferences(AppearanceService.DefaultPaletteId, "light", 15, null);
            _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 850));
            _transcript.ClearSelection();
            _transcript.ShowConversation(chat, [answer]);
            await WaitAsync("document.querySelector('.mermaid-block')?.dataset.diagramState==='ready' && document.querySelectorAll('.katex').length===2 && document.querySelectorAll('.content-block').length===2", "theme fixture loads code, table, diagram and real KaTeX");
            await EvalAsync<bool>("""
                (() => {
                  window.__appearanceBody=document.querySelector('.message-body');
                  window.__appearanceBlock=document.querySelector('.content-block');
                  window.__appearanceCode=__appearanceBlock.querySelector('code');
                  window.__appearanceDiagramViewport=document.querySelector('.mermaid-viewport');
                  window.__appearanceSvg=__appearanceDiagramViewport.querySelector('svg');
                  window.__appearanceAnchor=[...__appearanceBody.querySelectorAll(':scope > p')].find(p=>p.textContent.startsWith('THEME_ANCHOR'));
                  scrollTo(0,scrollY+__appearanceAnchor.getBoundingClientRect().top);
                  __appearanceBlock.querySelector('[data-block-action=fold]').click();
                  return true;
                })()
                """);
            await WaitAsync("!transcriptState().following && __appearanceBlock.dataset.collapsed==='true'", "reading fixture is folded and detached from bottom following");
            await EvalAsync<bool>("(() => { window.__appearanceTop=__appearanceAnchor.getBoundingClientRect().top; return true; })()");

            foreach (string themeMode in new[] { "light", "dark" })
            {
                foreach (var preset in AppearanceService.Palettes)
                {
                    AppearanceService.ApplyPreferences(preset.Id, themeMode, 15, null);
                    await WaitAppearanceAsync(AppearanceService.Current, 15);
                    await Task.Delay(150);
                    Check(await EvalAsync<bool>("__appearanceBody===document.querySelector('.message-body') && __appearanceBlock===document.querySelector('.content-block') && __appearanceCode===document.querySelector('.content-block code') && __appearanceBlock.dataset.collapsed==='true'"), "palette/mode changes preserve existing prose/code nodes and fold preference for " + preset.Id + "/" + themeMode);
                    await CheckAppearanceColorsAsync(AppearanceService.Current);
                    Check(await EvalAsync<bool>("document.querySelector('.mermaid-viewport svg')===__appearanceSvg && getComputedStyle(__appearanceDiagramViewport).backgroundColor==='rgb(255, 255, 255)' && getComputedStyle(__appearanceSvg.querySelector('.node rect')).fill==='rgb(237, 242, 252)' && getComputedStyle(__appearanceSvg.querySelector('.node rect')).stroke==='rgb(69, 104, 178)' && getComputedStyle(__appearanceSvg.querySelector('.node text')).fill==='rgb(36, 39, 45)'"), "diagram retains the same fixed light SVG and white canvas for " + preset.Id + "/" + themeMode);
                    Check(await EvalAsync<bool>("Math.abs(__appearanceAnchor.getBoundingClientRect().top-__appearanceTop)<2 && !transcriptState().following"), "color-only theme change retains the reading region");
                }
            }

            await CheckAppearanceFontSelectionAsync();
            await CheckAppearanceNavigationRaceAsync();
            await CheckAppearanceCacheAsync(chat, answer);
            await CheckAppearanceDiagramSelectionAsync();
            await CheckAppearanceHighContrastAsync();
            await CheckAppearanceDarkDiagramTypesAsync();
            await CheckAppearanceSystemAsync();
            Check(JsonSerializer.Serialize(answer.Message) == formal, "theme, font, native copy and cached navigation do not change formal source messages");
            _metrics["appearanceReadingScope"] = "native WinUI/WebView2; 5 presets x light/dark; 13/13.5/15/18/22px; 12 complete preview lines; real message/code nodes and selected text retained; cached A/B/A anchor; fixed white Mermaid content and stable SVG; synthetic high-contrast application palette; actual system mode";
        }
        catch (Exception error)
        {
            validationError = error;
            throw;
        }
        finally
        {
            // Restore each independent resource, and retain the first assertion if restoration also fails.
            // 独立恢复各项资源；若恢复也失败，保留最初断言并附加清理错误，不能覆盖首错或放宽验收。
            var cleanupErrors = new List<Exception>();
            async Task CleanupAsync(Func<Task> action)
            {
                try { await action(); }
                catch (Exception error) { cleanupErrors.Add(error); }
            }
            await CleanupAsync(async () => { await EvalAsync<bool>("(() => { window.kynxaDiagrams?.closeViewer(); getSelection().removeAllRanges(); return true; })()"); });
            await CleanupAsync(async () => { await _transcript.Browser.CoreWebView2.CallDevToolsProtocolMethodAsync("Emulation.setFocusEmulationEnabled", "{\"enabled\":false}"); });
            await CleanupAsync(() => { AppearanceService.ApplyPreferences(paletteId, mode, fontSize, customAccent); return Task.CompletedTask; });
            await CleanupAsync(() => { _window.AppWindow.Resize(size); return Task.CompletedTask; });
            if (cleanupErrors.Count > 0)
            {
                if (validationError is not null) cleanupErrors.Insert(0, validationError);
                throw new AggregateException("Appearance validation or restoration failed; the original validation error is first when present.", cleanupErrors);
            }
        }
    }

    private async Task CheckAppearanceNavigationRaceAsync()
    {
        foreach (string input in new[] { "keyboard-up", "keyboard-down", "wheel-up", "wheel-down", "pointer" })
        {
            AppearanceService.ApplyPreferences(AppearanceService.DefaultPaletteId, "light", 15, null);
            await WaitAppearanceAsync(AppearanceService.Current, 15);
            await Task.Delay(120);
            await EvalAsync<bool>($$"""
                (() => {
                  getSelection().removeAllRanges();
                  scrollTo(0, scrollY + __appearanceAnchor.getBoundingClientRect().top);
                  window.__appearanceNavigationRace={ran:false,input:{{JsonSerializer.Serialize(input)}}};
                  const handler=event=>{
                    if(event.data?.type!=='setAppearance' || event.data.fontSize!==18) return;
                    chrome.webview.removeEventListener('message',handler);
                    // Arrive after the production host handler and before its animation frame.
                    // 在正式宿主处理器之后、延后定位动画帧之前模拟新的阅读导航。
                    const state=__appearanceNavigationRace;
                    const delta=state.input.endsWith('-up')?-160:160;
                    if(state.input==='pointer') document.dispatchEvent(new PointerEvent('pointerdown',{button:0,bubbles:true}));
                    else window.dispatchEvent(state.input.startsWith('keyboard')
                      ?new KeyboardEvent('keydown',{key:delta<0?'PageUp':'PageDown',bubbles:true})
                      :new WheelEvent('wheel',{deltaY:delta,bubbles:true}));
                    state.before=scrollY;
                    scrollTo(0,Math.max(0,scrollY+delta));
                    state.expected=scrollY;state.ran=true;
                    if(state.input==='pointer') document.dispatchEvent(new PointerEvent('pointerup',{button:0,bubbles:true}));
                  };
                  chrome.webview.addEventListener('message',handler);
                  return true;
                })()
                """);
            AppearanceService.ApplyPreferences(AppearanceService.DefaultPaletteId, "light", 18, null);
            await WaitAppearanceAsync(AppearanceService.Current, 18);
            await WaitAsync("__appearanceNavigationRace.ran", "user navigation arrives during the deferred font anchor restore");
            await Task.Delay(180);
            var state = await EvalAsync<JsonElement>("({...__appearanceNavigationRace,after:scrollY,following:transcriptState().following})");
            _metrics["appearanceNavigationRace-" + input] = state;
            Check(Math.Abs(state.GetProperty("before").GetDouble() - state.GetProperty("expected").GetDouble()) > 100
                && Math.Abs(state.GetProperty("after").GetDouble() - state.GetProperty("expected").GetDouble()) < 2
                && !state.GetProperty("following").GetBoolean(),
                "new " + input + " navigation wins over a deferred font anchor restore");
        }
    }

    private async Task WaitAppearanceAsync(AppearancePalette palette, double fontSize)
    {
        await WaitAsync($$"""
            document.documentElement.dataset.theme==={{JsonSerializer.Serialize(palette.IsDark ? "dark" : "light")}}
            && document.documentElement.dataset.highContrast==={{JsonSerializer.Serialize(palette.IsHighContrast.ToString().ToLowerInvariant())}}
            && Math.abs(parseFloat(getComputedStyle(document.documentElement).fontSize)-{{fontSize.ToString(System.Globalization.CultureInfo.InvariantCulture)}})<.01
            && getComputedStyle(document.documentElement).getPropertyValue('--appearance-main').trim().toLowerCase()==={{JsonSerializer.Serialize(palette.Main.ToLowerInvariant())}}
            """, "native appearance payload reaches colors, scheme and chat font size");
    }

    private async Task CheckAppearanceColorsAsync(AppearancePalette palette)
    {
        var colors = await EvalAsync<JsonElement>("""
            (() => {
              const color=selector=>getComputedStyle(document.querySelector(selector)).color;
              return {main:getComputedStyle(document.documentElement).backgroundColor,body:color('.message-body'),
                formula:color('.katex'),quote:color('blockquote'),table:color('td'),
                keyword:color(".content-block code span[style='color:#8250df']"),comment:color(".content-block code span[style='color:#576f43']"),
                string:color(".content-block code span[style='color:#0a3069']"),number:color(".content-block code span[style='color:#0550ae']"),
                surface:getComputedStyle(document.querySelector('.math[data-display=true]')).backgroundColor,
                diagramHeader:getComputedStyle(document.querySelector('.mermaid-header')).backgroundColor,
                diagramSource:getComputedStyle(document.querySelector('.mermaid-source')).backgroundColor,diagramSourceText:color('.mermaid-source code'),
                codeSize:parseFloat(getComputedStyle(document.querySelector('.content-block code')).fontSize),
                tableSize:parseFloat(getComputedStyle(document.querySelector('table')).fontSize),scheme:getComputedStyle(document.documentElement).colorScheme};
            })()
            """);
        Check(colors.GetProperty("main").GetString() == Rgb(palette.Main)
            && colors.GetProperty("body").GetString() == Rgb(palette.Text)
            && colors.GetProperty("formula").GetString() == Rgb(palette.Text)
            && colors.GetProperty("table").GetString() == Rgb(palette.Text)
            && colors.GetProperty("quote").GetString() == Rgb(palette.Secondary)
            && colors.GetProperty("surface").GetString() == Rgb(palette.ReplySurface), "prose, formula, quote, table and surfaces share the effective semantic palette");
        Check(colors.GetProperty("diagramHeader").GetString() == Rgb(palette.Sidebar)
            && colors.GetProperty("diagramSource").GetString() == Rgb(palette.Sidebar)
            && colors.GetProperty("diagramSourceText").GetString() == Rgb(palette.Text),
            "diagram toolbar and original source follow the effective application palette");
        Check(colors.GetProperty("keyword").GetString() == Rgb(palette.CodeKeyword)
            && colors.GetProperty("comment").GetString() == Rgb(palette.CodeComment)
            && colors.GetProperty("string").GetString() == Rgb(palette.CodeString)
            && colors.GetProperty("number").GetString() == Rgb(palette.CodeNumber), "known inline code highlighting maps safely to readable theme token colors");
        Check(Math.Abs(colors.GetProperty("codeSize").GetDouble() - AppearanceService.ChatFontSize * 13 / 15) < .02
            && Math.Abs(colors.GetProperty("tableSize").GetDouble() - AppearanceService.ChatFontSize) < .02,
            "code/table font sizes follow the chat preference while retaining default code proportions");
        _metrics[$"appearanceColors-{palette.Id}-{palette.IsDark}-{palette.IsHighContrast}"] = colors;
    }

    private static string Rgb(string hex)
    {
        var color = AppearanceService.ParseColor(hex);
        return $"rgb({color.R}, {color.G}, {color.B})";
    }

    private async Task CheckAppearanceFontSelectionAsync()
    {
        await EvalAsync<bool>("""
            (() => {
              const node=__transcriptSmoke.textNode(__appearanceAnchor,'THEME_ANCHOR');
              getSelection().setBaseAndExtent(node,0,node,12);
              window.__appearanceSelection={node,text:getSelection().toString(),copy:transcriptSelectionText()};
              return true;
            })()
            """);
        foreach (double size in new[] { 13, 13.5, 15, 18, 22 })
        {
            AppearanceService.ApplyPreferences(AppearanceService.DefaultPaletteId, "dark", size, "#E58C38");
            await WaitAppearanceAsync(AppearanceService.Current, size);
            await Task.Delay(120);
            Check(await EvalAsync<bool>("getSelection().anchorNode===__appearanceSelection.node && getSelection().toString()===__appearanceSelection.text && transcriptSelectionText()===__appearanceSelection.copy && __transcriptSmoke.copyEvent()===__appearanceSelection.copy"), "font changes preserve exact selected text, DOM endpoints and copied prose at " + size);
            Check(await EvalAsync<bool>("Math.abs(__appearanceAnchor.getBoundingClientRect().top-__appearanceTop)<2 && !transcriptState().following && __appearanceBlock.dataset.collapsed==='true'"), "font change preserves the reading paragraph and folded block at " + size);
            var geometry = await EvalAsync<JsonElement>("""
                (() => {
                  const node=__appearanceCode.firstChild,pre=__appearanceBlock.querySelector('pre'),bounds=pre.getBoundingClientRect();
                  const line=token=>{const offset=node.textContent.indexOf(token),range=document.createRange();range.setStart(node,offset);range.setEnd(node,offset+3);return range.getBoundingClientRect();};
                  return {lastBottom:line('012 THEME_LINE').bottom,nextTop:line('013 THEME_LINE').top,clipBottom:bounds.top+pre.clientHeight,
                    page:document.documentElement.scrollWidth,width:innerWidth,code:parseFloat(getComputedStyle(__appearanceCode).fontSize)};
                })()
                """);
            Check(geometry.GetProperty("lastBottom").GetDouble() <= geometry.GetProperty("clipBottom").GetDouble() + 1
                && geometry.GetProperty("nextTop").GetDouble() >= geometry.GetProperty("clipBottom").GetDouble() - 1,
                "folded preview retains twelve complete visible source lines at " + size);
            Check(geometry.GetProperty("page").GetDouble() <= geometry.GetProperty("width").GetDouble() + 1,
                "font change leaves overflow within content/table scrollers at " + size);
            _metrics["appearanceFold-" + size] = geometry;
        }
        _transcript.ClearSelection();
        await WaitAsync("getSelection().isCollapsed", "clear the font-test selection");
        string source = await EvalAsync<string>("__appearanceCode.textContent");
        await EvalAsync<bool>("(() => { __appearanceBlock.querySelector('.copy-block').click(); return true; })()");
        await WaitAsync("__appearanceBlock.querySelector('.copy-block').dataset.copyState==='success'", "folded content receives a real native clipboard receipt at 22px");
        Check(await Clipboard.GetContent().GetTextAsync() == source, "folded content copies its full original forty lines after all font/theme changes");
        foreach (int width in new[] { 480, 980 })
        {
            _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(width, 850));
            await Task.Delay(180);
            var geometry = await EvalAsync<JsonElement>("""
                (() => ({page:document.documentElement.scrollWidth,width:innerWidth,
                  buttons:[...document.querySelectorAll('.content-block-header button')].filter(button=>!button.hidden).map(button=>{
                    const r=button.getBoundingClientRect(),h=button.closest('header').getBoundingClientRect();
                    return {action:button.dataset.blockAction||'copy',left:r.left,right:r.right,top:r.top,bottom:r.bottom,
                      headerLeft:h.left,headerRight:h.right,headerTop:h.top,headerBottom:h.bottom};
                  })}))()
                """);
            _metrics["appearanceLargeFontGeometry-" + width] = geometry;
            await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), $"kynxa-transcript-appearance-reading-{width}.png"));
            Check(geometry.GetProperty("page").GetDouble() <= geometry.GetProperty("width").GetDouble() + 1
                && geometry.GetProperty("buttons").EnumerateArray().All(button =>
                    button.GetProperty("left").GetDouble() >= button.GetProperty("headerLeft").GetDouble()
                    && button.GetProperty("right").GetDouble() <= button.GetProperty("headerRight").GetDouble() + 1
                    && button.GetProperty("top").GetDouble() >= button.GetProperty("headerTop").GetDouble()
                    && button.GetProperty("bottom").GetDouble() <= button.GetProperty("headerBottom").GetDouble() + 1),
                "large chat text keeps visible action controls within their rounded frame at width " + width);
            Check(await EvalAsync<bool>("(() => { const fold=document.querySelectorAll('.content-block')[1].querySelector('[data-block-action=fold]'); return fold.hidden && fold.getClientRects().length===0; })()"),
                "the short Python block continues to hide its unnecessary fold action at width " + width);
        }
    }

    private async Task CheckAppearanceCacheAsync(Guid chat, KYNXA_Desktop.ViewModels.ConversationMessageViewModel answer)
    {
        await EvalAsync<bool>("(() => { scrollTo(0,scrollY+__appearanceAnchor.getBoundingClientRect().top); __appearanceBlock.querySelector('[data-block-action=wrap]').click(); window.__appearanceCacheTop=__appearanceAnchor.getBoundingClientRect().top; return true; })()");
        var other = Guid.NewGuid();
        _transcript.ShowConversation(other, [Message(other, "assistant", "APPEARANCE_OTHER\n\n```txt\nother source\n```")]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('APPEARANCE_OTHER')", "appearance cache fixture opens B");
        AppearanceService.ApplyPreferences("teal", "light", 13.5, null);
        await WaitAppearanceAsync(AppearanceService.Current, 13.5);
        _transcript.ShowConversation(chat, [answer]);
        await WaitAsync("__appearanceBlock===document.querySelector('.content-block')", "cached A restores its real code nodes after global style change in B");
        await Task.Delay(200);
        Check(await EvalAsync<bool>("__appearanceBlock.dataset.wrap==='true' && __appearanceBlock.dataset.collapsed==='true' && !transcriptState().following && Math.abs(__appearanceAnchor.getBoundingClientRect().top-__appearanceCacheTop)<2"), "A/B/A restores reading anchor, wrap/fold and current font after a theme/size change in B");
        Check(await EvalAsync<bool>("parseFloat(getComputedStyle(__appearanceCode).fontSize)>11 && parseFloat(getComputedStyle(document.documentElement).fontSize)===13.5 && document.querySelectorAll('.content-block-reading-action').length===4"), "cached content inherits current font and never duplicates controls");
    }

    private async Task CheckAppearanceDiagramSelectionAsync()
    {
        await WaitAsync("document.querySelector('.mermaid-block')?.dataset.diagramState==='ready'", "the appearance selection fixture has a diagram");
        await EvalAsync<bool>("""
            (() => {
              const viewport=document.querySelector('.mermaid-viewport'),svg=viewport.querySelector('svg'),node=__transcriptSmoke.textNode(svg,'Start');
              getSelection().setBaseAndExtent(node,0,node,node.textContent.length);
              window.__appearanceDiagram={viewport,svg,node,selected:getSelection().toString(),scale:viewport.dataset.scale,
                transform:viewport.querySelector('.mermaid-canvas').style.transform};
              return true;
            })()
            """);
        AppearanceService.ApplyPreferences("violet", "dark", 18, null);
        await WaitAppearanceAsync(AppearanceService.Current, 18);
        await Task.Delay(350);
        Check(await EvalAsync<bool>("document.querySelector('.mermaid-viewport svg')===__appearanceDiagram.svg && getSelection().anchorNode===__appearanceDiagram.node && getSelection().toString()===__appearanceDiagram.selected && __appearanceDiagram.viewport.dataset.scale===__appearanceDiagram.scale && __appearanceDiagram.viewport.querySelector('.mermaid-canvas').style.transform===__appearanceDiagram.transform && getComputedStyle(__appearanceDiagram.viewport).backgroundColor==='rgb(255, 255, 255)' && getComputedStyle(__appearanceDiagram.svg.querySelector('.node rect')).fill==='rgb(237, 242, 252)'"), "dark/size change preserves the selected diagram label, same fixed light SVG, white canvas and zoom");
        _transcript.ClearSelection();
        await WaitAsync("getSelection().isCollapsed", "the appearance diagram selection clears");
        await Task.Delay(350);
        Check(await EvalAsync<bool>("document.querySelector('.mermaid-viewport svg')===__appearanceDiagram.svg && document.querySelector('.mermaid-block').dataset.diagramState==='ready' && __appearanceDiagram.viewport.dataset.scale===__appearanceDiagram.scale && __appearanceDiagram.viewport.querySelector('.mermaid-canvas').style.transform===__appearanceDiagram.transform && getComputedStyle(__appearanceDiagram.viewport).backgroundColor==='rgb(255, 255, 255)' && getComputedStyle(__appearanceDiagram.svg.querySelector('.node rect')).fill==='rgb(237, 242, 252)'"), "clearing the selected diagram retains the same fixed light SVG, white canvas and zoom");
        Check(await EvalAsync<bool>("document.querySelector('.mermaid-source code').textContent==='flowchart LR\\n  A[中文 Start] --> B[English 完成]'"), "appearance changes preserve the exact original diagram source");
    }

    private async Task CheckAppearanceHighContrastAsync()
    {
        var palette = AppearanceService.ResolveHighContrastPalette(AppearanceService.Palettes[1], type => type switch
        {
            UIElementType.Window => Color.FromArgb(255, 0, 0, 0),
            UIElementType.Highlight => Color.FromArgb(255, 255, 255, 0),
            UIElementType.HighlightText => Color.FromArgb(255, 0, 0, 0),
            UIElementType.Hotlight => Color.FromArgb(255, 0, 255, 255),
            _ => Color.FromArgb(255, 255, 255, 255)
        });
        // Feed a synthetic OS-resolved palette through the real host command, without toggling global accessibility.
        // 合成系统色通过真实宿主消息接入，不切换用户的全局辅助功能设置。
        await EvalAsync<bool>("(() => { window.__appearanceHighContrastSvg=document.querySelector('.mermaid-viewport svg'); return true; })()");
        _transcript.Browser.CoreWebView2.PostWebMessageAsJson(JsonSerializer.Serialize(new
        {
            type = "setAppearance", fontSize = 18,
            palette = new
            {
                main = palette.Main, soft = palette.Soft, accent = palette.Accent, selection = palette.Selection,
                text = palette.Text, secondary = palette.Secondary, border = palette.Border, sidebar = palette.Sidebar,
                muted = palette.Muted, success = palette.Success, error = palette.Error, warning = palette.Warning,
                replySurface = palette.ReplySurface, accentText = palette.AccentText, onAccent = palette.OnAccent,
                codeKeyword = palette.CodeKeyword, codeString = palette.CodeString, codeComment = palette.CodeComment,
                codeNumber = palette.CodeNumber, codeType = palette.CodeType, codeFunction = palette.CodeFunction,
                isDark = palette.IsDark, isHighContrast = palette.IsHighContrast
            }
        }));
        await WaitAppearanceAsync(palette, 18);
        await CheckAppearanceColorsAsync(palette);
        Check(await EvalAsync<bool>("getComputedStyle(document.documentElement).forcedColorAdjust==='none' && getComputedStyle(document.querySelector('td'),'::selection').color==='rgb(0, 0, 0)' && getComputedStyle(document.querySelector('td'),'::selection').backgroundColor==='rgb(255, 255, 0)'"), "high contrast honors system selected background/foreground without browser recoloring");
        Check(await EvalAsync<bool>("(() => { const viewport=document.querySelector('.mermaid-viewport'),svg=viewport.querySelector('svg'),rect=svg.querySelector('.node rect'); return svg===__appearanceHighContrastSvg && getComputedStyle(viewport).backgroundColor==='rgb(255, 255, 255)' && getComputedStyle(rect).fill==='rgb(237, 242, 252)' && getComputedStyle(rect).stroke==='rgb(69, 104, 178)' && getComputedStyle(svg.querySelector('.node text')).fill==='rgb(36, 39, 45)'; })()"), "high-contrast application colors retain the same fixed light diagram SVG and white canvas");
        await EvalAsync<bool>("(() => { document.querySelector('.mermaid-block').scrollIntoView({block:'center'}); return true; })()");
        await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), "kynxa-transcript-appearance-high-contrast.png"));
    }

    private async Task CheckAppearanceDarkDiagramTypesAsync()
    {
        AppearanceService.ApplyPreferences(AppearanceService.DefaultPaletteId, "dark", 18, null);
        await WaitAppearanceAsync(AppearanceService.Current, 18);
        string[] sources =
        [
            "flowchart LR\n A[中文 Start] --> B[完成 Done]",
            "sequenceDiagram\n participant A as 中文 UI\n participant B as English Gateway\n A->>B: 发送 Send\n B-->>A: 完成 Done",
            "stateDiagram-v2\n [*] --> Waiting\n Waiting --> Done\n Done --> [*]",
            "classDiagram\n class Model {\n +String value\n +generate()\n }\n class Gateway\n Gateway --> Model",
            "erDiagram\n USER ||--o{ MESSAGE : sends\n USER {\n int id PK\n }\n MESSAGE {\n string text\n }",
            "gantt\n title 中文 Schedule\n dateFormat YYYY-MM-DD\n section UI\n Design :done, d1, 2026-10-01, 2d\n Test :active, t1, after d1, 2d",
            "mindmap\n root((中文 Task))\n  Theme\n  English Tests"
        ];
        var chat = Guid.NewGuid();
        var rows = sources.Select(source => Message(chat, "assistant", MermaidFence(source))).ToArray();
        _transcript.ShowConversation(chat, rows);
        await WaitAsync("document.querySelectorAll('.mermaid-block').length===7 && [...document.querySelectorAll('.mermaid-block')].every(block=>block.dataset.diagramState==='ready')", "all seven supported Mermaid types render in dark application mode");
        foreach (var row in rows)
        {
            Check(await EvalAsync<bool>($$"""
                (() => {
                  const block={{DiagramBlockExpression(row)}},svg=block.querySelector('.mermaid-viewport svg');
                  return !!svg && svg.querySelectorAll('text,tspan').length>0 && svg.viewBox.baseVal.width>0
                    && !svg.querySelector('foreignObject,script,image,a')
                    && getComputedStyle(block.querySelector('.mermaid-viewport')).backgroundColor==='rgb(255, 255, 255)'
                    && block.querySelector('.mermaid-source code').textContent==={{JsonSerializer.Serialize(row.Message.Content[11..^4])}};
                })()
                """), "supported diagram in dark application mode keeps a white canvas, real text, inert SVG and exact original source");
        }
        await EvalAsync<bool>("(() => { scrollTo(0,0); return true; })()");
        await CaptureViewportAsync(Path.Combine(Path.GetTempPath(), "kynxa-transcript-appearance-dark-diagrams.png"));
    }

    private async Task CheckAppearanceSystemAsync()
    {
        await EvalAsync<bool>("(() => { window.__appearanceSystemSvgs=[...document.querySelectorAll('.mermaid-viewport svg')]; return true; })()");
        AppearanceService.ApplyPreferences(AppearanceService.DefaultPaletteId, "system", 15, null);
        await WaitAppearanceAsync(AppearanceService.Current, 15);
        bool expectedDark = new AccessibilitySettings().HighContrast
            ? AppearanceService.Current.IsDark
            : new UISettings().GetColorValue(UIColorType.Background).R < 128;
        Check(AppearanceService.Mode == "system" && AppearanceService.Current.IsDark == expectedDark,
            "system mode resolves the current OS scheme without changing system settings");
        Check(await EvalAsync<bool>("(() => { const svgs=[...document.querySelectorAll('.mermaid-viewport svg')]; return svgs.length===__appearanceSystemSvgs.length && svgs.length===7 && svgs.every((svg,index)=>svg===__appearanceSystemSvgs[index] && getComputedStyle(svg.closest('.mermaid-viewport')).backgroundColor==='rgb(255, 255, 255)'); })()"),
            "system mode keeps the same diagram SVG nodes and white content canvases");
        _transcript.JumpToLatest();
        await WaitAsync("transcriptState().following && __transcriptSmoke.bottomDistance()<4", "explicit latest remains functional after all theme and font changes");
    }
}
