using System.Diagnostics;
using System.IO.Compression;
using System.Net;
using System.Net.Sockets;
using System.Net.WebSockets;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Xml.Linq;
using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.Web.WebView2.Core;

namespace TranscriptUiSmoke
{
    public partial class App
    {
        private void LaunchWorkspacePreviewChecks(Stopwatch cold)
        {
            foreach (string name in new[] { "Colors", "Dimensions", "Controls" })
                Resources.MergedDictionaries.Add(new ResourceDictionary { Source = new Uri($"ms-appx:///Styles/{name}.xaml") });
            var fixture = new WorkspacePreviewFixture();
            ModelGatewayService.FixtureAddress = fixture.Address;
            var panel = new ConversationDocumentPanel();
            Microsoft.UI.Xaml.UIElement content = panel;
            bool chromeOnly = Environment.GetCommandLineArgs().Contains("--workspace-chrome-only", StringComparer.Ordinal);
            if (chromeOnly)
            {
                // Exercise the real nested sidebar offsets, rather than giving Chrome the whole client area.
                // 覆盖真实侧栏的嵌套偏移与剩余高度，不再让 Chrome 独占整个窗口客户区。
                var shell = new Microsoft.UI.Xaml.Controls.Grid();
                shell.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
                shell.ColumnDefinitions.Add(new() { Width = new GridLength(420) });
                shell.RowDefinitions.Add(new() { Height = new GridLength(64) });
                shell.RowDefinitions.Add(new() { Height = new GridLength(1, GridUnitType.Star) });
                var sidebar = new Microsoft.UI.Xaml.Controls.Grid { Margin = new Thickness(14, 10, 14, 14) };
                foreach (double height in new[] { 32d, 28d, 34d }) sidebar.RowDefinitions.Add(new() { Height = new GridLength(height) });
                sidebar.RowDefinitions.Add(new() { Height = new GridLength(1, GridUnitType.Star) });
                for (int index = 0; index < 3; index++)
                {
                    var header = new Microsoft.UI.Xaml.Controls.TextBlock { Text = new[] { "WORKSPACE_HEADER", "TASKS_COLLAPSED", "FILE_TABS" }[index] };
                    Microsoft.UI.Xaml.Controls.Grid.SetRow(header, index); sidebar.Children.Add(header);
                }
                Microsoft.UI.Xaml.Controls.Grid.SetRow(panel, 3); sidebar.Children.Add(panel);
                Microsoft.UI.Xaml.Controls.Grid.SetRow(sidebar, 1); Microsoft.UI.Xaml.Controls.Grid.SetColumn(sidebar, 1);
                shell.Children.Add(sidebar); content = shell;
            }
            _window = new Window { Title = "KYNXA workspace preview checks", Content = content };
            _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(chromeOnly ? 1100 : 800, 850));
            panel.Loaded += (_, _) => _ = RunWorkspacePreviewChecksAsync(panel, fixture, cold);
            _window.AppWindow.Show(activateWindow: false);
        }

        private async Task RunWorkspacePreviewChecksAsync(ConversationDocumentPanel panel, WorkspacePreviewFixture fixture, Stopwatch cold)
        {
            File.WriteAllText(_result, "RUNNING: native workspace preview checks");
            try
            {
                Guid conversation = Guid.NewGuid();
                if (Environment.GetEnvironmentVariable("KYNXA_WORKSPACE_DOCX_ONLY") == "1")
                {
                    await CheckDocxDocumentAsync(panel, fixture, conversation);
                    SaveWorkspaceMetrics(cold, "workspace-docx-only");
                    File.WriteAllText(_result, $"PASS: {_checks} DOCX workspace checks; actual text/table/image rendering, isolated resources, cached DOM and PDF fallback. No Chrome launched.");
                    return;
                }
                if (Environment.GetEnvironmentVariable("KYNXA_WORKSPACE_PPTX_ONLY") == "1")
                {
                    await CheckPptxDocumentAsync(panel, fixture, conversation);
                    SaveWorkspaceMetrics(cold, "workspace-pptx-only");
                    File.WriteAllText(_result, $"PASS: {_checks} PPTX workspace checks; actual slides, text/table/image rendering, controls/keyboard, fitted aspect ratio, cached page state, scoped attachment and PDF fallback. No Chrome launched.");
                    return;
                }
                if (Environment.GetEnvironmentVariable("KYNXA_WORKSPACE_XLSX_ONLY") == "1")
                {
                    await CheckXlsxDocumentAsync(panel, fixture, conversation);
                    SaveWorkspaceMetrics(cold, "workspace-xlsx-only");
                    File.WriteAllText(_result, $"PASS: {_checks} XLSX workspace checks; typed values, cached formulas, real sheet switching/scrolling, cached document state and PDF fallback. No Chrome launched.");
                    return;
                }
                if (Environment.GetCommandLineArgs().Contains("--workspace-chrome-only", StringComparer.Ordinal))
                {
                    await CheckNativeChromeAsync(panel, fixture, conversation);
                    bool inputOnly = Environment.GetEnvironmentVariable("KYNXA_WORKSPACE_INPUT_ONLY") == "1";
                    bool geometryOnly = Environment.GetEnvironmentVariable("KYNXA_WORKSPACE_GEOMETRY_ONLY") == "1";
                    File.WriteAllText(_result, inputOnly
                        ? $"PASS: {_checks} isolated child Chrome checks; real parent HWND chain, host viewport, OS click/Unicode typing/wheel and background visibility."
                        : geometryOnly
                        ? $"PASS: {_checks} native Chrome geometry checks; sidebar region recovery, owned popup bounds and ancestor visibility. No foreground, input or CSS resize validation."
                        : $"PASS: {_checks} native Chrome checks; actual HWND, parent/visibility/bounds and CDP page body. Capture: kynxa-native-chrome.png.");
                    return;
                }
                if (Environment.GetEnvironmentVariable("KYNXA_WORKSPACE_BACKGROUND_ONLY") == "1")
                {
                    await CheckBackgroundDocumentAsync(panel, fixture, conversation);
                    File.WriteAllText(_result, $"PASS: {_checks} background document checks; preparation and Mermaid complete after switching away, and returning preserves the DOM without another request.");
                    return;
                }
                panel.ShowResource(conversation, "report.md", "报告.md", userSelected: true);
                await WaitWorkspaceAsync(panel, false, "!!document.querySelector('h1') && !!document.querySelector('table') && !!document.querySelector('.katex') && !!document.querySelector('.mermaid-svg')");
                Check(fixture.LastUserSelected, "explicit picker admission reaches the gateway preview request");
                var document = panel.DocumentBrowser!.CoreWebView2;
                await document.ExecuteScriptAsync("window.__originalHeading=document.querySelector('h1'); scrollTo(0,900)");
                await Task.Delay(180);
                int prepared = fixture.PreviewRequests;
                panel.ShowResource(conversation, "notes.txt");
                await WaitWorkspaceAsync(panel, false, "document.querySelector('pre')?.textContent.includes('PLAIN_FILE_FIXTURE') === true");
                Check(await panel.DocumentBrowser!.CoreWebView2.ExecuteScriptAsync("getComputedStyle(document.querySelector('pre')).fontFamily.includes('Segoe UI Variable Text') && getComputedStyle(document.querySelector('pre')).fontFamily.includes('Microsoft YaHei UI')") == "true",
                    "plain Chinese and English document text uses the UI font stack rather than a browser monospace default");
                panel.ShowResource(conversation, "report.md", "报告.md", userSelected: true);
                await WaitWorkspaceAsync(panel, false, "!!document.querySelector('h1') && scrollY > 700");
                Check(ReferenceEquals(document, panel.DocumentBrowser!.CoreWebView2)
                    && await document.ExecuteScriptAsync("window.__originalHeading === document.querySelector('h1')") == "true"
                    && fixture.PreviewRequests == prepared + 1,
                    "switching back reuses the actual document DOM and does not repeat parsing or HTTP preview preparation");
                await CheckBackgroundDocumentAsync(panel, fixture, conversation);
                panel.ShowResource(conversation, "sample.cs");
                await WaitWorkspaceAsync(panel, false, "document.querySelectorAll('pre.source-code code span[style]').length > 0 && document.querySelector('pre.source-code').textContent.includes('public class Sample')");
                Check(await panel.DocumentBrowser!.CoreWebView2.ExecuteScriptAsync("getComputedStyle(document.querySelector('code')).fontFamily.includes('Cascadia Mono') && parseFloat(getComputedStyle(document.querySelector('code')).fontSize) >= 14") == "true",
                    "code previews use readable monospaced Latin with Chinese fallback at fourteen pixels");
                panel.ShowResource(conversation, "sample.rs");
                await WaitWorkspaceAsync(panel, false, "document.querySelectorAll('pre.source-code code span[style]').length > 0 && document.querySelector('code').textContent.includes('pub fn greet')");
                panel.ShowResource(conversation, "drawing.svg");
                await WaitWorkspaceAsync(panel, false, "document.querySelector('.image-view img')?.naturalWidth === 160 && !document.querySelector('pre.source-code')");
                panel.ShowResource(conversation, "flow.mmd");
                await WaitWorkspaceAsync(panel, false, "!!document.querySelector('.mermaid-svg') && document.querySelector('.mermaid-source').textContent.includes('FILE_DIAGRAM')");
                panel.ShowResource(conversation, "picture.png");
                await WaitWorkspaceAsync(panel, false, "document.querySelector('img')?.naturalWidth > 0");
                await CheckDocxDocumentAsync(panel, fixture, conversation);
                await CheckPptxDocumentAsync(panel, fixture, conversation);
                await CheckXlsxDocumentAsync(panel, fixture, conversation);
                panel.ShowResource(conversation, "sample.pdf");
                await WaitWorkspaceAsync(panel, false, "document.contentType === 'application/pdf' || !!document.querySelector('embed[type=\"application/pdf\"]')");
                Check(fixture.PdfRequests > 0, "PDF native rendering reads bytes from the authorized content endpoint");
                panel.ShowResource(conversation, "page.html");
                await WaitWorkspaceAsync(panel, true, "document.body.textContent.includes('ISOLATED_HTML_FIXTURE')");
                Check(!panel.WebsiteBrowser!.CoreWebView2.Settings.IsWebMessageEnabled
                    && !panel.WebsiteBrowser.CoreWebView2.Settings.AreHostObjectsAllowed,
                    "external and HTML pages expose no native message or host-object bridge");
                Check(await panel.WebsiteBrowser.CoreWebView2.ExecuteScriptAsync("document.querySelector('button').click(); document.querySelector('button').dataset.count === '1'") == "true",
                    "local HTML runs its own interactive controls inside the isolated document view");
                bool documentsOnly = Environment.GetEnvironmentVariable("KYNXA_WORKSPACE_DOCUMENTS_ONLY") == "1";
                if (!documentsOnly) await CheckNativeChromeAsync(panel, fixture, conversation);
                panel.ShowResource(conversation, "report.md", "报告.md", userSelected: true, version: "updated");
                await WaitWorkspaceAsync(panel, false, "!!document.querySelector('h1') && !!document.querySelector('.mermaid-svg')");
                Check(panel.DocumentBrowser.CoreWebView2.Settings.IsWebMessageEnabled && !panel.DocumentBrowser.CoreWebView2.Settings.AreHostObjectsAllowed,
                    "trusted local preview returns after PDF and external browsing without exposing host objects");
                SaveWorkspaceMetrics(cold, documentsOnly ? "workspace-documents-only" : "workspace-preview-only");
                File.WriteAllText(_result, documentsOnly
                    ? $"PASS: {_checks} workspace document checks; Markdown/table/math/Mermaid, background completion, text/code, image, SVG, DOCX/PDF fallback, isolated HTML and cached DOM. No Chrome launched."
                    : $"PASS: {_checks} native workspace preview checks; Markdown/table/math/Mermaid, scroll restoration, text, image, SVG, DOCX, PDF, isolated HTML, cached DOM and actual embedded Chrome HWND/CDP body.");
            }
            catch (Exception error) { File.WriteAllText(_result, "FAIL: " + error); }
            finally { panel.Dispose(); await fixture.DisposeAsync(); _window?.Close(); }
        }

        private void SaveWorkspaceMetrics(Stopwatch cold, string mode)
        {
            _metrics["checks"] = _checks; _metrics["mode"] = mode;
            _metrics["durationMs"] = cold.ElapsedMilliseconds;
            File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics));
        }

        private async Task CheckDocxDocumentAsync(ConversationDocumentPanel panel, WorkspacePreviewFixture fixture, Guid conversation)
        {
            const string frameBody = "document.querySelector('iframe.docx-view')?.contentDocument?.body";
            var firstDisplay = Stopwatch.StartNew();
            panel.ShowResource(conversation, "sample.docx", "文档.docx", userSelected: true);
            await WaitWorkspaceAsync(panel, false, $"{frameBody}?.textContent.includes('DOCX_BROWSER_FIXTURE') && {frameBody}?.querySelector('table')?.textContent.includes('DOCX_TABLE_CELL') && {frameBody}?.querySelector('img')?.naturalWidth > 0");
            _metrics["docxFirstDisplayMs"] = firstDisplay.ElapsedMilliseconds;
            var document = panel.DocumentBrowser!.CoreWebView2;
            Check(await document.ExecuteScriptAsync($"!{frameBody}.querySelector('script,iframe,object,[onclick]') && !{frameBody}.textContent.includes('ALT_CHUNK_EXECUTED') && document.querySelector('iframe.docx-view').sandbox.value === 'allow-same-origin'") == "true",
                "DOCX altChunks/scripts are absent and its document lives in a frame without script permission");
            Check(await document.ExecuteScriptAsync("document.querySelector('iframe.docx-view').getBoundingClientRect().width <= innerWidth && document.querySelector('iframe.docx-view').contentDocument.querySelector('.docx-wrapper').getBoundingClientRect().width <= innerWidth + 2") == "true",
                "DOCX pages fit the available sidebar width instead of forcing a wide horizontal viewport");
            await document.ExecuteScriptAsync($"window.__docxPage = {frameBody}.querySelector('section.docx')");
            int contentReads = fixture.DocxRequests;
            panel.ShowResource(conversation, "notes.txt");
            await WaitWorkspaceAsync(panel, false, "document.querySelector('pre')?.textContent.includes('PLAIN_FILE_FIXTURE') === true");
            int prepared = fixture.PreviewRequests;
            var cachedReturn = Stopwatch.StartNew();
            panel.ShowResource(conversation, "sample.docx", "文档.docx", userSelected: true);
            await WaitWorkspaceAsync(panel, false, $"!!window.__docxPage && window.__docxPage === {frameBody}?.querySelector('section.docx')");
            _metrics["docxCachedReturnMs"] = cachedReturn.ElapsedMilliseconds;
            Check(ReferenceEquals(document, panel.DocumentBrowser!.CoreWebView2) && fixture.DocxRequests == contentReads
                && fixture.PreviewRequests == prepared,
                "returning to DOCX retains the same page DOM and does not fetch or render its file again");
            fixture.AttachmentConversationId = conversation;
            var attachment = new ChatAttachment(fixture.AttachmentId, "attachment.docx",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document", 4096, Sha256: WorkspacePreviewFixture.AttachmentVersion);
            var attachmentDisplay = Stopwatch.StartNew();
            panel.ShowAttachment(conversation, fixture.AttachmentMessageId, attachment);
            await WaitWorkspaceAsync(panel, false, $"document.querySelector('iframe.docx-view')?.title === 'attachment.docx' && {frameBody}?.textContent.includes('DOCX_BROWSER_FIXTURE') && {frameBody}?.querySelector('table')?.textContent.includes('DOCX_TABLE_CELL')");
            Check(fixture.AttachmentPreviewRequests == 1 && fixture.AttachmentContentRequests == 1 && fixture.UnexpectedAttachmentRequests == 0,
                $"DOCX message attachments render through exact message/attachment/version scoped preview and content requests (preview={fixture.AttachmentPreviewRequests}, content={fixture.AttachmentContentRequests}, unexpected={fixture.UnexpectedAttachmentRequests})");
            _metrics["docxAttachmentDisplayMs"] = attachmentDisplay.ElapsedMilliseconds;
            panel.ShowResource(conversation, "broken.docx");
            await WaitWorkspaceAsync(panel, false, "document.contentType === 'application/pdf' || !!document.querySelector('embed[type=\"application/pdf\"]')");
            Check(fixture.BrokenDocxRequests == 1 && fixture.DocxFallbackRequests == 1 && fixture.PdfRequests > 0,
                "a failed direct DOCX render reports failure and receives its PDF representation once");
            Check(fixture.UnexpectedDocxRequests == 0, "DOCX external resources never reach the fixture network endpoint");
            _metrics["docxContentRequests"] = fixture.DocxRequests;
            _metrics["docxPdfFallbackRequests"] = fixture.DocxFallbackRequests;
            await CheckDeferredDocxTailAsync(panel, fixture, conversation);
        }

        private async Task CheckDeferredDocxTailAsync(ConversationDocumentPanel panel, WorkspacePreviewFixture fixture, Guid conversation)
        {
            panel.ShowResource(conversation, "sample.docx", "文档.docx", userSelected: true);
            await WaitWorkspaceAsync(panel, false, "document.querySelector('iframe.docx-view')?.contentDocument?.body.textContent.includes('DOCX_SCROLL_ROW_69') === true");
            var browser = panel.DocumentBrowser!;
            var document = browser.CoreWebView2;
            await document.ExecuteScriptAsync("window.__docxTailWaiting=false; const originalParse=window.docx.parseAsync; window.__docxTailGate=new Promise(resolve=>window.__releaseDocxTail=resolve); window.docx.parseAsync=async(...arguments)=>{const parsed=await originalParse(...arguments); const originalImage=parsed.loadDocumentImage.bind(parsed); parsed.loadDocumentImage=(...imageArguments)=>{window.__docxTailWaiting=true; return window.__docxTailGate.then(()=>originalImage(...imageArguments));}; window.docx.parseAsync=originalParse; return parsed;}");
            int beforeRead = fixture.DocxRequests;
            panel.ShowResource(conversation, "sample.docx", "文档.docx", userSelected: true, version: "slow-tail-v2");
            await WaitWorkspaceAsync(panel, false, "window.__docxTailWaiting && document.querySelector('iframe.docx-view')?.contentDocument?.body.textContent.includes('DOCX_SCROLL_ROW_69') === true");
            DateTime firstPaintDeadline = DateTime.UtcNow.AddSeconds(10);
            while (panel.PreparedResourceBrowser(conversation, "sample.docx") is null && DateTime.UtcNow < firstPaintDeadline)
                await Task.Delay(30);
            Check(panel.PreparedResourceBrowser(conversation, "sample.docx") is not null
                && panel.IsResourcePreparingInBackground(conversation, "sample.docx")
                && browser.Visibility == Visibility.Visible && browser.Opacity == 1 && browser.IsHitTestVisible,
                "DOCX releases its first-paint queue slot and enables input while a real document image is still pending");
            await SendWorkspacePointerAsync(browser, "iframe.docx-view", click: true, wheelY: -1440);
            await WaitWorkspaceAsync(panel, false, "document.querySelector('iframe.docx-view').contentWindow.scrollY > 0 && window.__docxTailWaiting");
            await document.ExecuteScriptAsync("window.__tailDocxPage=document.querySelector('iframe.docx-view').contentDocument.querySelector('section.docx')");
            panel.ShowResource(conversation, "notes.txt");
            await WaitWorkspaceAsync(panel, false, "document.querySelector('pre')?.textContent.includes('PLAIN_FILE_FIXTURE') === true");
            Check(panel.IsResourcePreparingInBackground(conversation, "sample.docx")
                && browser.Visibility == Visibility.Visible && browser.Opacity == 0 && !browser.IsHitTestVisible,
                "switching files keeps unfinished background resources alive without covering or taking input from the current document");
            await document.ExecuteScriptAsync("window.__releaseDocxTail()");
            DateTime settledDeadline = DateTime.UtcNow.AddSeconds(20);
            while (panel.IsResourcePreparingInBackground(conversation, "sample.docx") && DateTime.UtcNow < settledDeadline)
                await Task.Delay(40);
            Check(!panel.IsResourcePreparingInBackground(conversation, "sample.docx") && browser.Visibility == Visibility.Collapsed,
                "the hidden DOCX becomes eligible for suspension only after its real pending resource has settled");
            int contentReads = fixture.DocxRequests, previews = fixture.PreviewRequests;
            panel.ShowResource(conversation, "sample.docx", "文档.docx", userSelected: true, version: "slow-tail-v2");
            await WaitWorkspaceAsync(panel, false, "window.__tailDocxPage === document.querySelector('iframe.docx-view')?.contentDocument?.querySelector('section.docx') && document.querySelector('iframe.docx-view')?.contentDocument?.querySelector('img')?.naturalWidth > 0 && document.querySelector('iframe.docx-view').contentWindow.scrollY > 0");
            Check(ReferenceEquals(browser, panel.DocumentBrowser) && contentReads == beforeRead + 1
                && fixture.DocxRequests == contentReads && fixture.PreviewRequests == previews,
                "returning after background settling preserves the same DOCX DOM and scroll state without another raw read or render");
            _metrics["docxFirstPaintBeforeTail"] = true;
        }

        private async Task CheckPptxDocumentAsync(ConversationDocumentPanel panel, WorkspacePreviewFixture fixture, Guid conversation)
        {
            const string frameDocument = "document.querySelector('.pptx-view iframe')?.contentDocument";
            const string visibleSlide = "[...document.querySelector('.pptx-view iframe').contentDocument.querySelectorAll('.pptx-slide')].find(slide => getComputedStyle(slide).display !== 'none')";
            var firstDisplay = Stopwatch.StartNew();
            panel.ShowResource(conversation, "sample.pptx", "演示文稿.pptx", userSelected: true);
            await WaitWorkspaceAsync(panel, false, $"{frameDocument}?.body.textContent.includes('PPTX_SLIDE_ONE') && {frameDocument}?.querySelector('table')?.textContent.includes('PPTX_TABLE_CELL') && {frameDocument}?.querySelector('img')?.naturalWidth > 0");
            _metrics["pptxFirstDisplayMs"] = firstDisplay.ElapsedMilliseconds;
            var document = panel.DocumentBrowser!.CoreWebView2;
            Check(await document.ExecuteScriptAsync($"document.querySelector('.pptx-view iframe').sandbox.value === 'allow-same-origin' && !{frameDocument}.querySelector('script,object,iframe,[onclick]')") == "true",
                "PPTX slides render in a frame without script permission or executable document markup");
            Check(await document.ExecuteScriptAsync($"(() => {{ const slide=({visibleSlide}).getBoundingClientRect(); const frame=document.querySelector('.pptx-view iframe').getBoundingClientRect(); return slide.width > 0 && Math.abs(slide.width / slide.height - 16/9) < .02 && frame.width <= innerWidth + 2 && slide.width <= frame.width + 2; }})()") == "true",
                "PPTX fit preserves the presentation aspect ratio inside the actual sidebar viewport");
            Check(await document.ExecuteScriptAsync("document.querySelector('.pptx-view input[data-action=page]').value === '1' && document.querySelector('.pptx-view input[data-action=page]').max === '2'") == "true",
                "the presentation control reports page one of the two actual slides");
            Check(await document.ExecuteScriptAsync($"(() => {{ const slide=({visibleSlide}); const title=[...slide.querySelectorAll('.pptx-object')].find(node=>node.textContent.includes('PPTX_SLIDE_ONE')); const body=[...slide.querySelectorAll('.pptx-object')].find(node=>node.textContent.includes('PPTX_MASTER_BODY')); return title && body && Math.abs(parseFloat(title.style.left)-48)<1 && Math.abs(parseFloat(title.style.width)-1184)<1 && Math.abs(parseFloat(body.style.top)-480)<1; }})()") == "true",
                "slide placeholders inherit missing position and size through an empty layout from their master");
            await document.ExecuteScriptAsync("window.__officeProxyDenied=false; Promise.all(['document.pptx?version=forged','document.xlsx?version=fixture-v1'].map(path=>fetch('https://kynxa-workspace-content.local/Workspace/'+path).then(response=>response.ok).catch(()=>false))).then(results=>window.__officeProxyDenied=results.every(result=>!result))");
            await WaitWorkspaceAsync(panel, false, "window.__officeProxyDenied === true");
            Check(fixture.PptxRequests == 1 && fixture.XlsxRequests == 0,
                "the Office content proxy rejects another version or format without reading any additional authorized original");
            await SendWorkspacePointerAsync(panel.DocumentBrowser!, ".pptx-slide[style*='display: block']", click: true,
                frameSelector: ".pptx-view iframe");
            await WaitWorkspaceAsync(panel, false, $"document.querySelector('.pptx-view input[data-action=page]').value === '2' && ({visibleSlide}).textContent.includes('PPTX_SLIDE_TWO')");
            await SendWorkspacePointerAsync(panel.DocumentBrowser!, ".pptx-slide[style*='display: block']", click: true,
                frameSelector: ".pptx-view iframe");
            Check(await document.ExecuteScriptAsync("document.querySelector('.pptx-view input[data-action=page]').value === '2'") == "true",
                "clicking the actual slide advances one page and clicking the final page does not wrap");
            await SendWorkspacePointerAsync(panel.DocumentBrowser!, ".pptx-view button[data-action=previous]", click: true);
            await document.ExecuteScriptAsync($"(() => {{ const doc={frameDocument}; const text=({visibleSlide}).querySelector('.pptx-text'); const range=doc.createRange(); range.selectNodeContents(text); doc.getSelection().removeAllRanges(); doc.getSelection().addRange(range); }})()");
            await SendWorkspacePointerAsync(panel.DocumentBrowser!, ".pptx-slide[style*='display: block']", click: true,
                frameSelector: ".pptx-view iframe");
            Check(await document.ExecuteScriptAsync("document.querySelector('.pptx-view input[data-action=page]').value === '1'") == "true",
                "a click used to clear selected slide text does not accidentally advance the presentation");
            await document.ExecuteScriptAsync($"{frameDocument}.getSelection().removeAllRanges()");
            await SendWorkspacePointerAsync(panel.DocumentBrowser!, ".pptx-slide[style*='display: block']", click: true,
                frameSelector: ".pptx-view iframe", dragX: 36);
            Check(await document.ExecuteScriptAsync("document.querySelector('.pptx-view input[data-action=page]').value === '1'") == "true",
                "dragging across slide content preserves the page instead of treating a selection gesture as next");
            await document.ExecuteScriptAsync($"{frameDocument}.getSelection().removeAllRanges()");
            await SendWorkspacePointerAsync(panel.DocumentBrowser!, ".pptx-view button[data-action=next]", click: true);
            await WaitWorkspaceAsync(panel, false, $"document.querySelector('.pptx-view input[data-action=page]').value === '2' && ({visibleSlide}).textContent.includes('PPTX_SLIDE_TWO')");
            await SendWorkspacePointerAsync(panel.DocumentBrowser!, ".pptx-view button[data-action=previous]", click: true);
            await WaitWorkspaceAsync(panel, false, "document.querySelector('.pptx-view input[data-action=page]').value === '1'");
            SendChromeInput(new NativeChromeInput { Type = 1, Data = new() { Key = new() { VirtualKey = 0x22 } } },
                new NativeChromeInput { Type = 1, Data = new() { Key = new() { VirtualKey = 0x22, Flags = 0x0002 } } });
            await WaitWorkspaceAsync(panel, false, "document.querySelector('.pptx-view input[data-action=page]').value === '2'");
            await document.ExecuteScriptAsync($"window.__pptxView=document.querySelector('.pptx-view'); window.__pptxFirstSlide={frameDocument}.querySelector('.pptx-slide')");
            int contentReads = fixture.PptxRequests;
            panel.ShowResource(conversation, "notes.txt");
            await WaitWorkspaceAsync(panel, false, "document.querySelector('pre')?.textContent.includes('PLAIN_FILE_FIXTURE') === true");
            int prepared = fixture.PreviewRequests;
            panel.ShowResource(conversation, "sample.pptx", "演示文稿.pptx", userSelected: true);
            await WaitWorkspaceAsync(panel, false, $"window.__pptxView === document.querySelector('.pptx-view') && window.__pptxFirstSlide === {frameDocument}?.querySelector('.pptx-slide') && document.querySelector('.pptx-view input[data-action=page]').value === '2'");
            Check(ReferenceEquals(document, panel.DocumentBrowser!.CoreWebView2) && contentReads == 1
                && fixture.PptxRequests == contentReads && fixture.PreviewRequests == prepared,
                "returning to PPTX retains its actual DOM and selected slide without another raw fetch or preview preparation");
            fixture.AttachmentConversationId = conversation;
            var attachment = new ChatAttachment(fixture.PptxAttachmentId, "attachment.pptx",
                "application/vnd.openxmlformats-officedocument.presentationml.presentation", 4096, Sha256: WorkspacePreviewFixture.AttachmentVersion);
            panel.ShowAttachment(conversation, fixture.AttachmentMessageId, attachment);
            await WaitWorkspaceAsync(panel, false, $"document.querySelector('.pptx-view iframe')?.title === 'attachment.pptx' && {frameDocument}?.body.textContent.includes('PPTX_SLIDE_ONE') && {frameDocument}?.querySelector('table')?.textContent.includes('PPTX_TABLE_CELL')");
            Check(fixture.PptxAttachmentPreviewRequests == 1 && fixture.PptxAttachmentContentRequests == 1 && fixture.UnexpectedAttachmentRequests == 0,
                $"PPTX attachments use exact message, attachment and version scoped content addresses (preview={fixture.PptxAttachmentPreviewRequests}, content={fixture.PptxAttachmentContentRequests}, unexpected={fixture.UnexpectedAttachmentRequests})");
            panel.ShowResource(conversation, "broken.pptx");
            await WaitWorkspaceAsync(panel, false, "document.contentType === 'application/pdf' || !!document.querySelector('embed[type=\"application/pdf\"]')");
            Check(fixture.BrokenPptxRequests == 1 && fixture.PptxFallbackRequests == 1 && fixture.PdfRequests > 0,
                "invalid direct PPTX rendering falls back to its PDF representation once");
            panel.ShowResource(conversation, "late-failure.pptx");
            await WaitWorkspaceAsync(panel, false, $"document.querySelector('.pptx-view iframe')?.title === 'late-failure.pptx' && {frameDocument}?.body.textContent.includes('PPTX_SLIDE_ONE') && document.querySelector('.pptx-view input[data-action=page]')?.value === '1'");
            DateTime settledDeadline = DateTime.UtcNow.AddSeconds(10);
            while (panel.PreparedResourceBrowser(conversation, "late-failure.pptx") is null && DateTime.UtcNow < settledDeadline)
                await Task.Delay(30);
            Check(panel.PreparedResourceBrowser(conversation, "late-failure.pptx") is not null,
                "the late-page fixture has fully settled its first shown receipt before navigating");
            var lateDocument = panel.DocumentBrowser!.CoreWebView2;
            await SendWorkspacePointerAsync(panel.DocumentBrowser!, ".pptx-view button[data-action=next]", click: true, clicks: 2);
            await WaitWorkspaceAsync(panel, false, "document.contentType === 'application/pdf' || !!document.querySelector('embed[type=\"application/pdf\"]')");
            Check(fixture.LatePptxRequests == 1 && fixture.LatePptxFallbackRequests == 1,
                "a later unsupported slide after shown coalesces repeated errors into one PDF fallback");
            _metrics["pptxContentRequests"] = fixture.PptxRequests;
            _metrics["pptxPdfFallbackRequests"] = fixture.PptxFallbackRequests;
            string? samplePath = Environment.GetEnvironmentVariable("KYNXA_WORKSPACE_PPTX_SAMPLE_PATH");
            if (!string.IsNullOrWhiteSpace(samplePath))
                await CheckUserPptxSampleAsync(panel, fixture, conversation, samplePath);
        }

        private async Task CheckUserPptxSampleAsync(ConversationDocumentPanel panel, WorkspacePreviewFixture fixture,
            Guid conversation, string samplePath)
        {
            var sample = fixture.LoadUserPptxSample(samplePath);
            Check(sample.SlideCount >= 2, "the read-only user presentation contains at least two actual slide parts");
            string firstText = JsonSerializer.Serialize(sample.FirstPageText);
            string secondText = JsonSerializer.Serialize(sample.SecondPageText);
            const string visibleSlide = "[...document.querySelector('.pptx-view iframe').contentDocument.querySelectorAll('.pptx-slide')].find(slide => getComputedStyle(slide).display !== 'none')";
            panel.ShowResource(conversation, "user-sample.pptx", sample.Title, userSelected: true);
            await WaitWorkspaceAsync(panel, false, $"document.querySelector('.pptx-view iframe')?.contentDocument?.body.textContent.includes({firstText}) && document.querySelector('.pptx-view input[data-action=page]')?.value === '1'");
            var document = panel.DocumentBrowser!.CoreWebView2;
            Check(await document.ExecuteScriptAsync($"document.querySelector('.pptx-view input[data-action=page]').max === '{sample.SlideCount}'") == "true",
                "the real presentation page count matches its original OOXML slide list");
            string evidenceDirectory = Path.GetDirectoryName(_result)!;
            using (var capture = File.Create(Path.Combine(evidenceDirectory, "workspace-user-pptx-page1.png")))
                await document.CapturePreviewAsync(CoreWebView2CapturePreviewImageFormat.Png, capture.AsRandomAccessStream());
            await SendWorkspacePointerAsync(panel.DocumentBrowser!, ".pptx-view button[data-action=next]", click: true);
            await WaitWorkspaceAsync(panel, false, $"document.querySelector('.pptx-view input[data-action=page]').value === '2' && ({visibleSlide}).textContent.includes({secondText})");
            using (var capture = File.Create(Path.Combine(evidenceDirectory, "workspace-user-pptx-page2.png")))
                await document.CapturePreviewAsync(CoreWebView2CapturePreviewImageFormat.Png, capture.AsRandomAccessStream());
            Check(fixture.UserPptxRequests == 1 && fixture.UserPptxPreviewRequests == 1,
                "the user presentation is read once through its independent preview fixture without changing the original file");
            _metrics["userPptxSlideCount"] = sample.SlideCount;
            _metrics["userPptxContentRequests"] = fixture.UserPptxRequests;
        }

        private async Task CheckXlsxDocumentAsync(ConversationDocumentPanel panel, WorkspacePreviewFixture fixture, Guid conversation)
        {
            var firstDisplay = Stopwatch.StartNew();
            panel.ShowResource(conversation, "sample.xlsx", "工作簿.xlsx", userSelected: true);
            await WaitWorkspaceAsync(panel, false, "document.querySelector('.xlsx-cell[data-address=A1]')?.textContent.includes('XLSX_TYPED_TITLE') && document.querySelector('.xlsx-cell[data-address=B1]')?.textContent.trim() === '42.5' && document.querySelector('.xlsx-cell[data-address=C1]')?.textContent.trim() === '50' && document.querySelector('.xlsx-cell[data-address=D1]')?.textContent.includes('XLSX_SHARED_TEXT')");
            _metrics["xlsxFirstDisplayMs"] = firstDisplay.ElapsedMilliseconds;
            var document = panel.DocumentBrowser!.CoreWebView2;
            Check(await document.ExecuteScriptAsync("document.querySelector('.xlsx-grid').getAttribute('role') === 'grid' && document.querySelector('.xlsx-cell[data-address=C1]').dataset.formula === 'SUM(B1,7.5)' && document.querySelector('.xlsx-cell[data-address=C2]').dataset.formula === '2+2' && document.querySelector('.xlsx-cell[data-address=C2]').textContent.trim() !== '4'") == "true",
                "Excel shows typed cells and stored formula results without recalculating uncached expressions");
            Check(await document.ExecuteScriptAsync("/^(true|真)$/i.test(document.querySelector('.xlsx-cell[data-address=A2]').textContent.trim()) && document.querySelector('.xlsx-cell[data-address=B2]').textContent.trim() === '01-01-24' && document.querySelector('.xlsx-cell[data-address=D2]').textContent.includes('<img') && !document.querySelector('.xlsx-grid img,.xlsx-grid script')") == "true",
                "Excel preserves Boolean/date data and treats markup-looking cell values as inert text");
            string cellFont = await document.ExecuteScriptAsync("getComputedStyle(document.querySelector('.xlsx-cell[data-address=B1]')).fontFamily");
            Check(cellFont.Contains("Segoe UI", StringComparison.Ordinal), "Excel preserves the declared cell font instead of the browser serif default: " + cellFont);
            using (var capture = File.Create(Path.Combine(Path.GetDirectoryName(_result)!, "workspace-xlsx-grid.png")))
                await document.CapturePreviewAsync(CoreWebView2CapturePreviewImageFormat.Png, capture.AsRandomAccessStream());
            // Target populated column L; the viewer can also expose empty columns beyond the used range.
            // 滚动到有数据的 L 列；预览器允许继续展示已用区域以外的空白列，不能把最右端当成最后一列数据。
            await document.ExecuteScriptAsync("const zoom=document.querySelector('.xlsx-view select[data-action=zoom]'); zoom.value='1.25'; zoom.dispatchEvent(new Event('change',{bubbles:true})); const area=document.querySelector('.xlsx-scroll'); const columnWidth=document.querySelector('.xlsx-cell[data-address=B1]').getBoundingClientRect().width; area.scrollLeft=columnWidth*8");
            await SendWorkspacePointerAsync(panel.DocumentBrowser!, ".xlsx-scroll", wheelY: -1440);
            await WaitWorkspaceAsync(panel, false, "document.querySelector('.xlsx-scroll').scrollLeft > 0 && document.querySelector('.xlsx-scroll').scrollTop >= 450 && [...document.querySelectorAll('.xlsx-cell')].some(cell=>cell.textContent.includes('SCROLL_ROW_'))");
            await SendWorkspacePointerAsync(panel.DocumentBrowser!, ".xlsx-view button[data-action=sheet][data-sheet-index=\"1\"]", click: true);
            await WaitWorkspaceAsync(panel, false, "document.querySelector('.xlsx-cell[data-address=A1]')?.textContent.includes('XLSX_SECOND_SHEET')");
            await document.ExecuteScriptAsync("window.__xlsxView=document.querySelector('.xlsx-view')");
            int contentReads = fixture.XlsxRequests;
            panel.ShowResource(conversation, "notes.txt");
            await WaitWorkspaceAsync(panel, false, "document.querySelector('pre')?.textContent.includes('PLAIN_FILE_FIXTURE') === true");
            int prepared = fixture.PreviewRequests;
            panel.ShowResource(conversation, "sample.xlsx", "工作簿.xlsx", userSelected: true);
            await WaitWorkspaceAsync(panel, false, "window.__xlsxView === document.querySelector('.xlsx-view') && document.querySelector('.xlsx-cell[data-address=A1]')?.textContent.includes('XLSX_SECOND_SHEET') && document.querySelector('.xlsx-view select[data-action=zoom]').value === '1.25'");
            Check(ReferenceEquals(document, panel.DocumentBrowser!.CoreWebView2) && contentReads == 1
                && fixture.XlsxRequests == contentReads && fixture.PreviewRequests == prepared,
                "returning to Excel retains its actual view, active sheet and zoom without refetching or parsing the workbook");
            await SendWorkspacePointerAsync(panel.DocumentBrowser!, ".xlsx-view button[data-action=sheet][data-sheet-index=\"0\"]", click: true);
            await WaitWorkspaceAsync(panel, false, "document.querySelector('.xlsx-scroll').scrollLeft > 0 && document.querySelector('.xlsx-scroll').scrollTop >= 450 && [...document.querySelectorAll('.xlsx-cell')].some(cell=>cell.textContent.includes('SCROLL_ROW_'))");
            await SendWorkspacePointerAsync(panel.DocumentBrowser!, ".xlsx-view button[data-action=sheet][data-sheet-index=\"2\"]", click: true);
            await WaitWorkspaceAsync(panel, false, "document.querySelector('.xlsx-cell[data-address=A1]')?.textContent.includes('DENSE_1_1')");
            await document.ExecuteScriptAsync("(() => { const zoom=document.querySelector('.xlsx-view select[data-action=zoom]'); zoom.value='0.5'; zoom.dispatchEvent(new Event('change',{bubbles:true})); })()");
            await WaitWorkspaceAsync(panel, false, "(() => { const area=document.querySelector('.xlsx-scroll'); const rect=area.getBoundingClientRect(); const cell=document.elementFromPoint(rect.left+area.clientWidth-3,rect.top+area.clientHeight-3)?.closest('.xlsx-cell'); return document.querySelectorAll('.xlsx-cell').length > 4096 && cell?.textContent.includes('DENSE_'); })()");
            Check(await document.ExecuteScriptAsync("(() => { const area=document.querySelector('.xlsx-scroll'); const rect=area.getBoundingClientRect(); const headers=[...document.querySelectorAll('.xlsx-column-header')]; return Math.max(...headers.map(node=>node.getBoundingClientRect().right)) >= rect.left+area.clientWidth-1; })()") == "true",
                "dense Excel at fifty-percent zoom fills the entire visible grid and rightmost headers beyond the old 4096-cell cut-off");
            await SendWorkspacePointerAsync(panel.DocumentBrowser!, ".xlsx-view button[data-action=sheet][data-sheet-index=\"1\"]", click: true);
            await WaitWorkspaceAsync(panel, false, "document.querySelector('.xlsx-cell[data-address=A1]')?.textContent.includes('XLSX_SECOND_SHEET') && ![...document.querySelectorAll('.xlsx-cell')].some(cell=>cell.textContent.includes('DENSE_'))");
            panel.ShowResource(conversation, "broken.xlsx");
            await WaitWorkspaceAsync(panel, false, "document.contentType === 'application/pdf' || !!document.querySelector('embed[type=\"application/pdf\"]')");
            Check(fixture.BrokenXlsxRequests == 1 && fixture.XlsxFallbackRequests == 1 && fixture.PdfRequests > 0,
                "invalid direct XLSX rendering falls back to its PDF representation once");
            _metrics["xlsxContentRequests"] = fixture.XlsxRequests;
            _metrics["xlsxPdfFallbackRequests"] = fixture.XlsxFallbackRequests;
        }

        private async Task CheckBackgroundDocumentAsync(ConversationDocumentPanel panel, WorkspacePreviewFixture fixture, Guid conversation)
        {
            panel.ShowResource(conversation, "slow.md");
            await fixture.SlowStarted.Task.WaitAsync(TimeSpan.FromSeconds(15));
            panel.ShowResource(conversation, "notes.txt");
            await WaitWorkspaceAsync(panel, false, "document.querySelector('pre')?.textContent.includes('PLAIN_FILE_FIXTURE') === true");
            var foreground = panel.DocumentBrowser!.CoreWebView2;
            fixture.ReleaseSlow.TrySetResult();
            DateTime deadline = DateTime.UtcNow.AddSeconds(35);
            while (panel.PreparedResourceBrowser(conversation, "slow.md") is null && DateTime.UtcNow < deadline) await Task.Delay(40);
            var background = panel.PreparedResourceBrowser(conversation, "slow.md")?.CoreWebView2;
            Check(background is not null && ReferenceEquals(foreground, panel.DocumentBrowser!.CoreWebView2),
                "switching away lets the previous request finish and cache its renderer without replacing the selected document");
            Check(await background!.ExecuteScriptAsync("!!document.querySelector('.mermaid-svg') && document.querySelector('h1').textContent === 'BACKGROUND_DOCUMENT'") == "true",
                "the background document finishes its actual diagram rendering before becoming inactive");
            await background.ExecuteScriptAsync("window.__backgroundHeading=document.querySelector('h1')");
            panel.ShowResource(conversation, "slow.md");
            await WaitWorkspaceAsync(panel, false, "window.__backgroundHeading === document.querySelector('h1')");
            Check(fixture.SlowRequests == 1 && ReferenceEquals(background, panel.DocumentBrowser!.CoreWebView2),
                "returning to the completed background document preserves its DOM and issues no duplicate preview request");
        }

        private async Task WaitWorkspaceAsync(ConversationDocumentPanel panel, bool website, string expression)
        {
            File.WriteAllText(_result, "RUNNING: workspace " + expression);
            DateTime deadline = DateTime.UtcNow.AddSeconds(30);
            string lastSource = "uninitialized";
            while (DateTime.UtcNow < deadline)
            {
                var core = (website ? panel.WebsiteBrowser : panel.DocumentBrowser)?.CoreWebView2;
                if (core is not null)
                {
                    lastSource = core.Source;
                    try
                    {
                        string json = await core.ExecuteScriptAsync("(() => !!(" + expression + "))()").AsTask().WaitAsync(TimeSpan.FromSeconds(5));
                        if (json == "true") { _checks++; return; }
                    }
                    catch (Exception error) when (error is InvalidOperationException or System.Runtime.InteropServices.COMException) { }
                }
                await Task.Delay(40);
            }
            string diagnostics = "unavailable";
            try
            {
                var core = (website ? panel.WebsiteBrowser : panel.DocumentBrowser)?.CoreWebView2;
                if (core is not null) diagnostics = await core.ExecuteScriptAsync("JSON.stringify({docx:typeof window.docx,zip:typeof window.JSZip,office:typeof window.kynxaOffice,pptx:typeof window.kynxaPptx,xlsx:typeof window.kynxaXlsx,frame:!!document.querySelector('iframe.docx-view'),body:document.querySelector('iframe.docx-view')?.contentDocument?.body?.textContent?.slice(0,600),pptxBody:document.querySelector('.pptx-view iframe')?.contentDocument?.body?.textContent?.slice(0,600),pptxPage:document.querySelector('.pptx-view input[data-action=page]')?.value,xlsxCells:[...document.querySelectorAll('.xlsx-cell')].slice(0,8).map(cell=>({address:cell.dataset.address,text:cell.textContent})),errors:[...document.querySelectorAll('[role=alert],.preview-error,.error')].map(node=>node.textContent.slice(0,200)),image:[...document.querySelector('iframe.docx-view')?.contentDocument?.querySelectorAll('img')||[]].map(i=>({src:i.src.slice(0,140),width:i.naturalWidth,complete:i.complete})),outer:document.body.textContent.slice(0,250)})");
                diagnostics += "; notice=" + string.Join(" | ", WorkTabVisuals(panel).OfType<Microsoft.UI.Xaml.Controls.TextBlock>()
                    .Where(text => text.Visibility == Visibility.Visible).Select(text => text.Text).Where(text => !string.IsNullOrEmpty(text)));
            }
            catch (Exception error) { diagnostics = error.Message; }
            throw new InvalidOperationException("Workspace preview timed out: " + expression + "; source=" + lastSource + "; diagnostics=" + diagnostics);
        }

        private async Task CheckNativeChromeAsync(ConversationDocumentPanel panel, WorkspacePreviewFixture fixture, Guid conversation)
        {
            File.WriteAllText(_result, "RUNNING: native Chrome open");
            nint owner = Microsoft.UI.Win32Interop.GetWindowFromWindowId(_window!.AppWindow.Id);
            bool inputOnly = Environment.GetEnvironmentVariable("KYNXA_WORKSPACE_INPUT_ONLY") == "1";
            bool geometryOnly = Environment.GetEnvironmentVariable("KYNXA_WORKSPACE_GEOMETRY_ONLY") == "1";
            if (inputOnly || !geometryOnly) await ActivateNativeTestHostAsync(owner);
            panel.SetPreviewEnabled(false);
            string site = Environment.GetEnvironmentVariable("KYNXA_WORKSPACE_TEST_SITE") ?? new Uri(fixture.Address, "/site").AbsoluteUri;
            panel.ShowWebsite(conversation, site, activate: false);
            panel.SetPreviewEnabled(true);
            await Task.Delay(120);
            Check(panel.ChromeBrowser is null, "restoring a website tab does not start its browser before explicit activation");
            panel.SetPreviewEnabled(false);
            panel.ShowWebsite(conversation, site);
            await Task.Delay(120);
            panel.SetPreviewEnabled(true);
            DateTime deadline = DateTime.UtcNow.AddSeconds(20);
            while (panel.ChromeBrowser?.BrowserWindow is not > 0 && DateTime.UtcNow < deadline) await Task.Delay(40);
            string notices = string.Join("; ", panel.Children.OfType<Microsoft.UI.Xaml.Controls.TextBlock>().Select(item => item.Text));
            Check(panel.ChromeBrowser?.BrowserWindow is > 0, "the actual owned Chrome HWND is embedded in the workspace; " + notices);
            nint window = panel.ChromeBrowser!.BrowserWindow;
            if (inputOnly)
            {
                // Verify the current child host directly; legacy top-level owner and region checks do not apply.
                // 直接验证当前真实子窗口宿主；旧的顶层 owner 与手工区域断言不适用于这条入口。
                while ((panel.ChromeBrowser.HostWindow == 0 || !NativeChromeIsChild(owner, window)
                    || !NativeChromeWindowVisible(window)) && DateTime.UtcNow < deadline) await Task.Delay(40);
                nint host = panel.ChromeBrowser.HostWindow;
                Check(host != 0 && NativeChromeGetParent(host) == owner && NativeChromeGetParent(window) == host
                    && NativeChromeIsChild(owner, window) && (NativeChromeGetStyle(window, -16).ToInt64() & 0x40000000L) != 0,
                    "Chrome is a real child of its viewport host inside the isolated application HWND");
                Check(NativeChromeWindowVisible(host) && NativeChromeWindowVisible(window)
                    && NativeChromeClientRect(host, out var client) && client.Right > 100 && client.Bottom > 100,
                    "the native child viewport and its browser are visible with usable client dimensions");
                NativeChromeClientRect(host, out var viewport);
                var origin = new NativeChromePoint();
                Check(NativeChromeClientToScreen(host, ref origin), "the child host client origin maps to real screen coordinates");
                var hostBounds = new NativeChromeRectangle { Left = origin.X, Top = origin.Y,
                    Right = origin.X + viewport.Right, Bottom = origin.Y + viewport.Bottom };
                string fixtureBody = await fixture.ChromeBodyAsync();
                while (!fixtureBody.Contains("BROWSER_SITE_FIXTURE", StringComparison.Ordinal) && DateTime.UtcNow < deadline)
                { await Task.Delay(40); fixtureBody = await fixture.ChromeBodyAsync(); }
                Check(fixtureBody.Contains("BROWSER_SITE_FIXTURE", StringComparison.Ordinal), "the isolated child browser has loaded its local input fixture");
                DateTime viewportDeadline = DateTime.UtcNow.AddSeconds(2);
                double cssWidth = 0, cssHeight = 0, dpr = 0;
                bool viewportMatches;
                do
                {
                    using var geometry = JsonDocument.Parse(await fixture.ChromeInteractionStateAsync());
                    cssWidth = geometry.RootElement.GetProperty("width").GetDouble();
                    cssHeight = geometry.RootElement.GetProperty("height").GetDouble();
                    dpr = geometry.RootElement.GetProperty("dpr").GetDouble();
                    viewportMatches = Math.Abs(cssWidth * dpr - viewport.Right) <= 3
                        && Math.Abs(cssHeight * dpr - viewport.Bottom) <= 3;
                    if (viewportMatches || DateTime.UtcNow >= viewportDeadline) break;
                    await Task.Delay(40);
                } while (true);
                var renderer = NativeChromeVisibleRenderRect(window);
                Check(viewportMatches, $"page CSS viewport matches the child host before OS input: CSS {cssWidth}x{cssHeight}, DPR {dpr}, host {viewport.Right}x{viewport.Bottom}; NativeWindowRenderRect={renderer.Window:X}/{renderer.Bounds.Left},{renderer.Bounds.Top},{renderer.Bounds.Right},{renderer.Bounds.Bottom}");
                await CheckNativeChromeInputAsync(fixture, owner, hostBounds);
                await CheckNativeChromeVisibilityAsync(panel, fixture, owner, window);
                return;
            }
            while ((NativeChromeOwner(window, 4) != owner || !NativeChromeWindowVisible(window)
                || !NativeChromeWindowRect(window, out var bounds) || bounds.Right - bounds.Left < 100)
                && DateTime.UtcNow < deadline) await Task.Delay(40);
            Check(NativeChromeOwner(window, 4) == owner && NativeChromeWindowVisible(window)
                && NativeChromeWindowRect(window, out var rectangle) && rectangle.Right - rectangle.Left > 100 && rectangle.Bottom - rectangle.Top > 100,
                "Chrome is a visible owned window with actual workspace dimensions");
            await CheckNativeChromeViewportAsync(panel, fixture, owner, window);
            NativeChromeSetRegion(window, 0, true);
            await Task.Delay(1200);
            await CheckNativeChromeViewportAsync(panel, fixture, owner, window);
            await CheckNativeChromePopupAsync(fixture, window);
            if (geometryOnly)
            {
                panel.Visibility = Visibility.Collapsed;
                await Task.Delay(1200);
                Check(!NativeChromeWindowVisible(window), $"a collapsed XAML ancestor hides its retained native browser; loaded={panel.ChromeBrowser!.IsLoaded}; root={panel.ChromeBrowser.XamlRoot is not null}; panel={panel.Visibility}; active={typeof(ConversationChromeView).GetField("_active", System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic)!.GetValue(panel.ChromeBrowser)}; callbacks={((System.Collections.IList)typeof(ConversationChromeView).GetField("_visibilityCallbacks", System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic)!.GetValue(panel.ChromeBrowser)!).Count}");
                panel.Visibility = Visibility.Visible;
                await Task.Delay(1200);
                Check(NativeChromeWindowVisible(window), "restoring the XAML ancestor reveals the same browser with its clipped viewport");
                await CheckNativeChromeViewportAsync(panel, fixture, owner, window);
                return;
            }
            if (Environment.GetEnvironmentVariable("KYNXA_WORKSPACE_VISIBILITY_ONLY") == "1")
            {
                await CheckNativeChromeVisibilityAsync(panel, fixture, owner, window);
                return;
            }
            // A renderer HWND can be an invisible IME helper; interaction follows the visible clipped viewport.
            // Renderer HWND 可能是不可见的输入法辅助窗口，交互坐标以实际可见裁剪视口为准。
            NativeChromeWindowRect(window, out var initialFrame);
            NativeChromePosition(window, 0, 0, 0, initialFrame.Right - initialFrame.Left + 160,
                initialFrame.Bottom - initialFrame.Top + 600, 0x0002 | 0x0004 | 0x0010);
            await Task.Delay(1200);
            NativeChromeWindowRect(window, out var recoveredFrame);
            Check(recoveredFrame.Right - recoveredFrame.Left == initialFrame.Right - initialFrame.Left
                && recoveredFrame.Bottom - recoveredFrame.Top == initialFrame.Bottom - initialFrame.Top,
                "an external Chrome resize is corrected without accumulating renderer-size gaps");
            await CheckNativeChromeViewportAsync(panel, fixture, owner, window);
            NativeChromeSetStyle(window, -16, (nint)(NativeChromeGetStyle(window, -16).ToInt64() | 0x00C00000L | 0x00040000L));
            await Task.Delay(1200);
            Check((NativeChromeGetStyle(window, -16).ToInt64() & (0x00C00000L | 0x00040000L | 0x40000000L)) == 0,
                "Chrome frame changes are corrected after embedding without requiring a XAML resize");
            File.WriteAllText(_result, "RUNNING: native Chrome body");
            string body = await fixture.ChromeBodyAsync();
            bool external = Environment.GetEnvironmentVariable("KYNXA_WORKSPACE_TEST_SITE") is not null;
            bool Loaded() => external ? body.Length > 20 : body.Contains("BROWSER_SITE_FIXTURE", StringComparison.Ordinal);
            while (!Loaded() && DateTime.UtcNow < deadline)
            {
                await Task.Delay(50); body = await fixture.ChromeBodyAsync();
            }
            await fixture.CaptureChromeAsync(Path.Combine(Path.GetTempPath(), "kynxa-native-chrome.png"));
            Check(Loaded(), external ? "the actual external website has rendered nonempty page content" : "the actual native Chrome CDP target contains the fixture page");
            if (!external) await CheckNativeChromeInputAsync(fixture, owner, NativeChromeViewportBounds(window));
            await CheckNativeChromeVisibilityAsync(panel, fixture, owner, window);
            panel.SetPreviewEnabled(false);
            Check(!NativeChromeWindowVisible(window), "collapsing the workspace immediately hides its Chrome window");
            panel.SetPreviewEnabled(true);
        }

        private static NativeChromeRectangle NativeChromeViewportBounds(nint browser)
        {
            nint region = NativeChromeCreateRegion(0, 0, 0, 0);
            try
            {
                NativeChromeWindowRect(browser, out var frame); NativeChromeGetRegion(browser, region);
                NativeChromeRegionBounds(region, out var clip);
                return new NativeChromeRectangle { Left = frame.Left + clip.Left, Top = frame.Top + clip.Top,
                    Right = frame.Left + clip.Right, Bottom = frame.Top + clip.Bottom };
            }
            finally { NativeChromeDeleteRegion(region); }
        }

        // Renderer bounds diagnose layout mismatches only; DOM input coordinates still use the actual host viewport.
        // Renderer 边界只用于诊断布局差异；DOM 交互坐标仍以真实宿主视口为准，不修改点击期望。
        private static (nint Window, NativeChromeRectangle Bounds) NativeChromeVisibleRenderRect(nint browser)
        {
            (nint Window, NativeChromeRectangle Bounds) largest = default;
            long largestArea = 0;
            NativeChromeEnumChildren(browser, (window, _) =>
            {
                if (!NativeChromeWindowVisible(window)) return true;
                var name = new StringBuilder(128);
                NativeChromeGetClass(window, name, name.Capacity);
                if (name.ToString() != "Chrome_RenderWidgetHostHWND" || !NativeChromeWindowRect(window, out var bounds)) return true;
                int width = bounds.Right - bounds.Left, height = bounds.Bottom - bounds.Top;
                if (width >= 100 && height >= 100 && (long)width * height > largestArea)
                { largest = (window, bounds); largestArea = (long)width * height; }
                return true;
            }, 0);
            return largest;
        }

        private async Task CheckNativeChromePopupAsync(WorkspacePreviewFixture fixture, nint browser)
        {
            var viewport = NativeChromeViewportBounds(browser);
            var popup = await fixture.CreateChromePopupAsync(viewport.Left, viewport.Top - 80);
            nint popupWindow = 0;
            try
            {
                DateTime deadline = DateTime.UtcNow.AddSeconds(5);
                while (popupWindow == 0 && DateTime.UtcNow < deadline)
                {
                    NativeChromeEnumWindows((window, _) =>
                    {
                        NativeChromeWindowThread(window, out uint process);
                        if (process != fixture.ChromeProcessId || window == browser || !NativeChromeWindowRect(window, out var bounds)) return true;
                        if (Math.Abs(bounds.Left - popup.Bounds.Left) <= 8 && Math.Abs(bounds.Top - popup.Bounds.Top) <= 8
                            && Math.Abs(bounds.Right - bounds.Left - popup.Bounds.Width) <= 8)
                        { popupWindow = window; return false; }
                        return true;
                    }, 0);
                    if (popupWindow == 0) await Task.Delay(40);
                }
                Check(popupWindow != 0, "the popup regression owns a second real Chrome window in its isolated browser process");
                NativeChromeSetStyle(popupWindow, -8, browser);
                NativeChromePosition(popupWindow, 0, viewport.Left, viewport.Top - 80, 320, 160, 0x0004 | 0x0010);
                NativeChromeShowWindow(popupWindow, 4);
                await Task.Delay(1200);
                NativeChromeWindowRect(popupWindow, out var moved);
                var clipped = NativeChromeViewportBounds(popupWindow);
                Check(moved.Top >= viewport.Top && moved.Left >= viewport.Left
                    && clipped.Right <= viewport.Right && clipped.Bottom <= viewport.Bottom,
                    "an owned Chrome popup stays inside the webpage instead of covering the task and file-tab rows");
            }
            finally { await fixture.CloseChromePopupAsync(popup.TargetId); }
        }

        private async Task CheckNativeChromeViewportAsync(ConversationDocumentPanel panel, WorkspacePreviewFixture fixture, nint owner, nint browser)
        {
            var view = panel.ChromeBrowser!;
            var point = view.TransformToVisual(view.XamlRoot.Content).TransformPoint(new Windows.Foundation.Point());
            var origin = new NativeChromePoint(); NativeChromeClientToScreen(owner, ref origin);
            double scale = view.XamlRoot.RasterizationScale;
            int left = origin.X + (int)Math.Round(point.X * scale), top = origin.Y + (int)Math.Round(point.Y * scale);
            int width = (int)Math.Round(view.ActualWidth * scale), height = (int)Math.Round(view.ActualHeight * scale);
            nint region = NativeChromeCreateRegion(0, 0, 0, 0);
            try
            {
                await Task.Delay(250);
                NativeChromeWindowRect(browser, out var frame);
                Check(NativeChromeGetRegion(browser, region) > 0 && NativeChromeRegionBounds(region, out var clip) > 0,
                    "the embedded browser has a native clipping region");
                NativeChromeRegionBounds(region, out var clipped);
                Check(Math.Abs(frame.Left + clipped.Left - left) <= 2 && Math.Abs(frame.Top + clipped.Top - top) <= 2
                    && Math.Abs(clipped.Right - clipped.Left - width) <= 2 && Math.Abs(clipped.Bottom - clipped.Top - height) <= 2,
                    $"browser clipping respects nested headers, tabs and sidebar margins: expected {left},{top},{width},{height}; actual {frame.Left + clipped.Left},{frame.Top + clipped.Top},{clipped.Right - clipped.Left},{clipped.Bottom - clipped.Top}");
                // A locked desktop can suspend renderer resize frames; this mode validates native geometry only.
                // 锁屏可能暂停 renderer 尺寸帧，此显式模式只验证原生几何，不把旧 CSS 视口当成渲染通过。
                if (Environment.GetEnvironmentVariable("KYNXA_WORKSPACE_GEOMETRY_ONLY") == "1") return;
                using var geometry = JsonDocument.Parse(await fixture.ChromeInteractionStateAsync());
                double dpr = geometry.RootElement.GetProperty("dpr").GetDouble();
                double cssWidth = geometry.RootElement.GetProperty("width").GetDouble(), cssHeight = geometry.RootElement.GetProperty("height").GetDouble();
                Check(Math.Abs(cssWidth * dpr - width) <= 3 && Math.Abs(cssHeight * dpr - height) <= 3,
                    $"page CSS viewport matches the actual sidebar without stretching: CSS {cssWidth}x{cssHeight}, DPR {dpr}, native {width}x{height}; frame {frame.Left},{frame.Top},{frame.Right},{frame.Bottom}; decoration {typeof(ConversationChromeView).GetField("_chromeUiInsets", System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic)!.GetValue(view)}");
            }
            finally { NativeChromeDeleteRegion(region); }
        }

        private async Task CheckNativeChromeVisibilityAsync(ConversationDocumentPanel panel, WorkspacePreviewFixture fixture, nint owner, nint browser)
        {
            File.WriteAllText(_result, "RUNNING: native Chrome background visibility");
            await fixture.ScrollChromeToTopAsync();
            const string script = "Add-Type -AssemblyName System.Windows.Forms; $f=New-Object System.Windows.Forms.Form; $f.Text='KYNXA isolated foreground fixture'; $f.StartPosition=[System.Windows.Forms.FormStartPosition]::Manual; $f.Location=[System.Drawing.Point]::new(0,0); $f.Width=300; $f.Height=140; $null=$f.ShowDialog()";
            var start = new ProcessStartInfo("powershell.exe") { UseShellExecute = false, CreateNoWindow = true };
            foreach (string argument in new[] { "-NoProfile", "-Command", script }) start.ArgumentList.Add(argument);
            using var foreground = Process.Start(start) ?? throw new InvalidOperationException("The isolated foreground fixture did not start.");
            try
            {
                nint other = 0;
                DateTime deadline = DateTime.UtcNow.AddSeconds(15);
                while (other == 0 && DateTime.UtcNow < deadline)
                {
                    NativeChromeEnumWindows((window, _) =>
                    {
                        NativeChromeWindowThread(window, out uint processId);
                        if (processId == foreground.Id && NativeChromeWindowVisible(window)) { other = window; return false; }
                        return true;
                    }, 0);
                    if (other == 0) await Task.Delay(40);
                }
                Check(other != 0, "the background check owns a separate foreground process without user documents");
                NativeChromeSetForeground(other);
                await Task.Delay(1200);
                Check(NativeChromeForeground() == other, "another application owns the foreground during the background rendering check");
                Check(NativeChromeWindowVisible(browser) && (NativeChromeGetStyle(browser, -20).ToInt64() & 0x8) == 0,
                    "losing foreground keeps the real Chrome page visible with normal owned-window z-order");
                Check((await fixture.ChromeBodyAsync()).Contains("BROWSER_SITE_FIXTURE", StringComparison.Ordinal),
                    "the background browser retains its loaded page instead of replacing it with a blank view");
                // Capture actual desktop pixels while the isolated host is in the background, not a CDP render.
                // 隔离宿主处于后台时截取实际屏幕像素，不用 CDP 离屏图代替画面证据。
                if (!NativeChromeWindowRect(owner, out var frame)) throw new InvalidOperationException("The isolated host has no capture bounds.");
                string capturePath = Path.Combine(Path.GetDirectoryName(_result)!, "workspace-chrome-background.png");
                string captureScript = $$"""
                    $ErrorActionPreference='Stop'
                    Add-Type -AssemblyName System.Drawing
                    Add-Type 'using System; using System.Runtime.InteropServices; public static class WorkspaceCaptureDpi { [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context); }'
                    $null=[WorkspaceCaptureDpi]::SetThreadDpiAwarenessContext([intptr](-4))
                    $image=[System.Drawing.Bitmap]::new({{frame.Right - frame.Left}},{{frame.Bottom - frame.Top}})
                    $graphics=[System.Drawing.Graphics]::FromImage($image)
                    try {
                        $graphics.CopyFromScreen({{frame.Left}},{{frame.Top}},0,0,$image.Size)
                        $image.Save('{{capturePath.Replace("'", "''")}}',[System.Drawing.Imaging.ImageFormat]::Png)
                    } finally { $graphics.Dispose(); $image.Dispose() }
                    """;
                var captureStart = new ProcessStartInfo("powershell.exe") { UseShellExecute = false, CreateNoWindow = true };
                foreach (string argument in new[] { "-NoProfile", "-NonInteractive", "-EncodedCommand", Convert.ToBase64String(Encoding.Unicode.GetBytes(captureScript)) })
                    captureStart.ArgumentList.Add(argument);
                using (var capture = Process.Start(captureStart) ?? throw new InvalidOperationException("The isolated screen capture did not start."))
                {
                    await capture.WaitForExitAsync();
                    if (capture.ExitCode != 0) throw new InvalidOperationException("The isolated background screen capture failed.");
                }
                NativeChromeShowWindow(owner, 6);
                await Task.Delay(350);
                Check(!NativeChromeWindowVisible(browser), "minimizing the owner hides its browser window");
                NativeChromeShowWindow(owner, 9);
                _window!.Activate();
                await Task.Delay(350);
                Check(NativeChromeWindowVisible(browser), "restoring the owner restores the existing browser page");
                panel.SetPreviewEnabled(false);
                Check(!NativeChromeWindowVisible(browser), "switching away from the webpage tab hides its browser window");
                panel.SetPreviewEnabled(true);
                await Task.Delay(150);
                Check(NativeChromeWindowVisible(browser), "switching back reveals the same browser window");
            }
            finally
            {
                if (!foreground.HasExited) foreground.Kill(entireProcessTree: true);
            }
        }

        private async Task SendWorkspacePointerAsync(Microsoft.UI.Xaml.Controls.WebView2 browser, string selector,
            bool click = false, int clicks = 1, int wheelY = 0, string? frameSelector = null, int dragX = 0)
        {
            nint owner = Microsoft.UI.Win32Interop.GetWindowFromWindowId(_window!.AppWindow.Id);
            await ActivateNativeTestHostAsync(owner);
            Check(browser.Visibility == Visibility.Visible && browser.Opacity > 0 && browser.IsHitTestVisible,
                "the selected document view accepts real pointer input before interacting");
            string boundsJson = await browser.CoreWebView2.ExecuteScriptAsync($"JSON.stringify((() => {{ const frame={JsonSerializer.Serialize(frameSelector)} ? document.querySelector({JsonSerializer.Serialize(frameSelector)}) : null; const element=(frame?.contentDocument || document).querySelector({JsonSerializer.Serialize(selector)}); if(!element) throw new Error('Missing input target'); const rect=element.getBoundingClientRect(); const offset=frame?.getBoundingClientRect() || {{left:0,top:0}}; return {{ x:Math.max(8,Math.min(innerWidth-8,offset.left+rect.left+rect.width/2)), y:Math.max(8,Math.min(innerHeight-8,offset.top+rect.top+rect.height/2)), width:innerWidth, height:innerHeight }}; }})())");
            using var bounds = JsonDocument.Parse(JsonSerializer.Deserialize<string>(boundsJson)!);
            var position = browser.TransformToVisual(_window.Content).TransformPoint(new Windows.Foundation.Point());
            double raster = browser.XamlRoot.RasterizationScale;
            var origin = new NativeChromePoint();
            Check(NativeChromeClientToScreen(owner, ref origin), "the document test client maps to screen coordinates");
            int x = origin.X + (int)Math.Round((position.X + bounds.RootElement.GetProperty("x").GetDouble()
                * browser.ActualWidth / bounds.RootElement.GetProperty("width").GetDouble()) * raster);
            int y = origin.Y + (int)Math.Round((position.Y + bounds.RootElement.GetProperty("y").GetDouble()
                * browser.ActualHeight / bounds.RootElement.GetProperty("height").GetDouble()) * raster);
            nint hit = NativeChromeWindowFromPoint(new NativeChromePoint { X = x, Y = y });
            Check(hit == owner || NativeChromeIsChild(owner, hit),
                $"real document input stays inside the isolated test window: target={selector}, point={x},{y}, owner={owner:X}, hit={hit:X}");
            var input = new List<NativeChromeInput> { ChromeMouseMove(x, y) };
            if (click)
                for (int index = 0; index < clicks; index++)
                {
                    input.Add(new NativeChromeInput { Data = new() { Mouse = new() { Flags = 0x0002 } } });
                    if (dragX != 0) input.Add(ChromeMouseMove(x + dragX, y));
                    input.Add(new NativeChromeInput { Data = new() { Mouse = new() { Flags = 0x0004 } } });
                }
            if (wheelY != 0)
                input.Add(new NativeChromeInput { Data = new() { Mouse = new() { Flags = 0x0800, MouseData = unchecked((uint)wheelY) } } });
            // SendInput validates the native hit/focus path; script execution above only reads the target geometry.
            // SendInput 验证真实原生命中与焦点路径；上方脚本只读取目标几何信息，不模拟点击或滚轮。
            SendChromeInput(input.ToArray());
            await Task.Delay(100);
        }

        private async Task CheckNativeChromeInputAsync(WorkspacePreviewFixture fixture, nint owner, NativeChromeRectangle bounds)
        {
            File.WriteAllText(_result, "RUNNING: native Chrome operating-system mouse and keyboard input");
            await ActivateNativeTestHostAsync(owner);
            using var geometry = JsonDocument.Parse(await fixture.ChromeInteractionStateAsync());
            double scale = (bounds.Right - bounds.Left) / geometry.RootElement.GetProperty("width").GetDouble();
            string lastHit = "";
            void Click(string name)
            {
                var area = geometry.RootElement.GetProperty(name);
                int x = bounds.Left + (int)Math.Round((area.GetProperty("x").GetDouble() + area.GetProperty("width").GetDouble() / 2) * scale);
                int y = bounds.Top + (int)Math.Round((area.GetProperty("y").GetDouble() + area.GetProperty("height").GetDouble() / 2) * scale);
                SendChromeInput(ChromeMouseMove(x, y));
                var className = new StringBuilder(128);
                nint hit = NativeChromeWindowFromPoint(new NativeChromePoint { X = x, Y = y });
                NativeChromeGetClass(hit, className, className.Capacity);
                lastHit = $"{hit:X}/{className}";
                SendChromeInput(new NativeChromeInput { Data = new() { Mouse = new() { Flags = 0x0002 } } },
                    new NativeChromeInput { Data = new() { Mouse = new() { Flags = 0x0004 } } });
            }
            Click("input");
            await Task.Delay(120);
            var keys = new List<NativeChromeInput>();
            foreach (char character in "abc中文")
            {
                keys.Add(new NativeChromeInput { Type = 1, Data = new() { Key = new() { Scan = character, Flags = 0x0004 } } });
                keys.Add(new NativeChromeInput { Type = 1, Data = new() { Key = new() { Scan = character, Flags = 0x0006 } } });
            }
            SendChromeInput(keys.ToArray());
            await Task.Delay(250);
            using var typed = JsonDocument.Parse(await fixture.ChromeInteractionStateAsync());
            bool inputWorked = typed.RootElement.GetProperty("value").GetString() == "abc中文";
            string typedEvidence = typed.RootElement.GetRawText() + "; hit=" + lastHit;
            Click("button");
            await Task.Delay(120);
            using var clicked = JsonDocument.Parse(await fixture.ChromeInteractionStateAsync());
            bool buttonWorked = clicked.RootElement.GetProperty("clicked").GetString() == "1";
            SendChromeInput(ChromeMouseMove(bounds.Left + 180, bounds.Top + 250),
                new NativeChromeInput { Data = new() { Mouse = new() { Flags = 0x0800, MouseData = unchecked((uint)-720) } } });
            await Task.Delay(350);
            using var scrolled = JsonDocument.Parse(await fixture.ChromeInteractionStateAsync());
            File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-native-chrome-input.json"),
                JsonSerializer.Serialize(new { inputWorked, buttonWorked, input = typedEvidence, clicked = clicked.RootElement.Clone(), scrolled = scrolled.RootElement.Clone() }));
            Check(inputWorked, "real OS mouse click and Unicode keyboard input reach the embedded Chrome input; " + typedEvidence);
            Check(buttonWorked, "real OS mouse input activates the embedded page button");
            Check(scrolled.RootElement.GetProperty("scrollY").GetDouble() > 0, "real OS mouse wheel scrolls the embedded Chrome page");
        }

        private async Task ActivateNativeTestHostAsync(nint owner)
        {
            _window!.Activate();
            NativeChromeSetForeground(owner);
            await Task.Delay(200);
            if (NativeChromeForeground() != owner)
            {
                // Real activation is required by Windows foreground rules; do not bypass page input through CDP.
                // Windows 前台规则要求真实激活；仅点自己的测试标题栏，不以 CDP 模拟页面输入。
                NativeChromePosition(owner, -1, 0, 0, 0, 0, 0x0001 | 0x0002 | 0x0010);
                NativeChromeWindowRect(owner, out var frame);
                SendChromeInput(ChromeMouseMove(frame.Left + 160, frame.Top + 16), new NativeChromeInput { Data = new() { Mouse = new() { Flags = 0x0002 } } },
                    new NativeChromeInput { Data = new() { Mouse = new() { Flags = 0x0004 } } });
                await Task.Delay(200);
                NativeChromePosition(owner, -2, 0, 0, 0, 0, 0x0001 | 0x0002 | 0x0010);
            }
            if (NativeChromeForeground() != owner)
            {
                uint foregroundThread = NativeChromeWindowThread(NativeChromeForeground(), out _);
                uint fixtureThread = NativeChromeCurrentThread();
                bool attached = NativeChromeAttachInput(fixtureThread, foregroundThread, true);
                try { NativeChromeSetForeground(owner); }
                finally { if (attached) NativeChromeAttachInput(fixtureThread, foregroundThread, false); }
                await Task.Delay(120);
            }
            nint foreground = NativeChromeForeground();
            Check(foreground == owner || NativeChromeIsChild(owner, foreground),
                $"the isolated browser test owns the foreground before sending real input; owner={owner:X}, foreground={foreground:X}");
        }

        private static void SendChromeInput(params NativeChromeInput[] inputs)
        {
            if (NativeChromeSendInput((uint)inputs.Length, inputs, Marshal.SizeOf<NativeChromeInput>()) != inputs.Length)
                throw new InvalidOperationException("Windows did not accept the isolated Chrome test input.");
        }

        private static NativeChromeInput ChromeMouseMove(int x, int y) => new()
        {
            Data = new() { Mouse = new()
            {
                X = (int)Math.Round((x - NativeChromeSystemMetric(76)) * 65535d / (NativeChromeSystemMetric(78) - 1)),
                Y = (int)Math.Round((y - NativeChromeSystemMetric(77)) * 65535d / (NativeChromeSystemMetric(79) - 1)),
                Flags = 0xc001
            } }
        };

        [StructLayout(LayoutKind.Sequential)] private struct NativeChromeRectangle { public int Left, Top, Right, Bottom; }
        [DllImport("user32.dll", EntryPoint = "IsChild")] private static extern bool NativeChromeIsChild(nint parent, nint child);
        [DllImport("user32.dll", EntryPoint = "IsWindowVisible")] private static extern bool NativeChromeWindowVisible(nint window);
        [DllImport("user32.dll", EntryPoint = "GetWindowRect")] private static extern bool NativeChromeWindowRect(nint window, out NativeChromeRectangle rectangle);
        [DllImport("user32.dll", EntryPoint = "GetClientRect")] private static extern bool NativeChromeClientRect(nint window, out NativeChromeRectangle rectangle);
        [DllImport("user32.dll", EntryPoint = "ClientToScreen")] private static extern bool NativeChromeClientToScreen(nint window, ref NativeChromePoint point);
        private delegate bool NativeChromeCallback(nint window, nint state);
        [DllImport("user32.dll", EntryPoint = "GetParent")] private static extern nint NativeChromeGetParent(nint window);
        [DllImport("user32.dll", EntryPoint = "EnumChildWindows")] private static extern bool NativeChromeEnumChildren(nint window, NativeChromeCallback callback, nint state);
        [DllImport("user32.dll", EntryPoint = "EnumWindows")] private static extern bool NativeChromeEnumWindows(NativeChromeCallback callback, nint state);
        [DllImport("user32.dll", EntryPoint = "ShowWindow")] private static extern bool NativeChromeShowWindow(nint window, int command);
        [DllImport("user32.dll", EntryPoint = "GetClassNameW", CharSet = CharSet.Unicode)] private static extern int NativeChromeGetClass(nint window, StringBuilder value, int length);
        [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")] private static extern nint NativeChromeGetStyle(nint window, int index);
        [DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW")] private static extern nint NativeChromeSetStyle(nint window, int index, nint value);
        [StructLayout(LayoutKind.Sequential)] private struct NativeChromeInput { public uint Type; public NativeChromeInputData Data; }
        [StructLayout(LayoutKind.Explicit)] private struct NativeChromeInputData
        {
            [FieldOffset(0)] public NativeChromeMouseInput Mouse;
            [FieldOffset(0)] public NativeChromeKeyInput Key;
        }
        [StructLayout(LayoutKind.Sequential)] private struct NativeChromeMouseInput { public int X, Y; public uint MouseData, Flags, Time; public nuint Extra; }
        [StructLayout(LayoutKind.Sequential)] private struct NativeChromeKeyInput { public ushort VirtualKey, Scan; public uint Flags, Time; public nuint Extra; }
        [DllImport("user32.dll", EntryPoint = "SetForegroundWindow")] private static extern bool NativeChromeSetForeground(nint window);
        [DllImport("user32.dll", EntryPoint = "GetForegroundWindow")] private static extern nint NativeChromeForeground();
        [DllImport("user32.dll", EntryPoint = "SendInput")] private static extern uint NativeChromeSendInput(uint count, NativeChromeInput[] inputs, int size);
        [DllImport("user32.dll", EntryPoint = "SetWindowPos")] private static extern bool NativeChromePosition(nint window, nint after, int x, int y, int width, int height, uint flags);
        [StructLayout(LayoutKind.Sequential)] private struct NativeChromePoint { public int X, Y; }
        [DllImport("user32.dll", EntryPoint = "WindowFromPoint")] private static extern nint NativeChromeWindowFromPoint(NativeChromePoint point);
        [DllImport("user32.dll", EntryPoint = "GetWindowThreadProcessId")] private static extern uint NativeChromeWindowThread(nint window, out uint processId);
        [DllImport("kernel32.dll", EntryPoint = "GetCurrentThreadId")] private static extern uint NativeChromeCurrentThread();
        [DllImport("user32.dll", EntryPoint = "AttachThreadInput")] private static extern bool NativeChromeAttachInput(uint thread, uint other, bool attach);
        [DllImport("user32.dll", EntryPoint = "GetSystemMetrics")] private static extern int NativeChromeSystemMetric(int index);
        [DllImport("user32.dll", EntryPoint = "GetWindow")] private static extern nint NativeChromeOwner(nint window, uint kind);
        [DllImport("user32.dll", EntryPoint = "GetWindowRgn")] private static extern int NativeChromeGetRegion(nint window, nint region);
        [DllImport("user32.dll", EntryPoint = "SetWindowRgn")] private static extern int NativeChromeSetRegion(nint window, nint region, bool redraw);
        [DllImport("gdi32.dll", EntryPoint = "CreateRectRgn")] private static extern nint NativeChromeCreateRegion(int left, int top, int right, int bottom);
        [DllImport("gdi32.dll", EntryPoint = "GetRgnBox")] private static extern int NativeChromeRegionBounds(nint region, out NativeChromeRectangle rectangle);
        [DllImport("gdi32.dll", EntryPoint = "DeleteObject")] private static extern bool NativeChromeDeleteRegion(nint region);
    }

    // The fixture owns loopback HTTP and its isolated Chrome process, without product data or model requests.
    // 夹具仅持有回环 HTTP 和独立 Chrome 测试进程，不读取正式数据或调用模型。
    internal sealed class WorkspacePreviewFixture : IAsyncDisposable
    {
        private readonly TcpListener _listener = new(IPAddress.Loopback, 0);
        private readonly CancellationTokenSource _stop = new();
        private readonly Task _server;
        private Process? _chrome;
        private string? _chromeWebSocket;
        private string? _chromeTabId;
        public Uri Address { get; }
        public bool LastUserSelected { get; private set; }
        public int PdfRequests { get; private set; }
        public int PreviewRequests { get; private set; }
        public int SlowRequests { get; private set; }
        public int DocxRequests { get; private set; }
        public int BrokenDocxRequests { get; private set; }
        public int DocxFallbackRequests { get; private set; }
        public int UnexpectedDocxRequests { get; private set; }
        public int PptxRequests { get; private set; }
        public int UserPptxRequests { get; private set; }
        public int UserPptxPreviewRequests { get; private set; }
        private byte[]? _userPptxSample;
        public int BrokenPptxRequests { get; private set; }
        public int PptxFallbackRequests { get; private set; }
        public int LatePptxRequests { get; private set; }
        public int LatePptxFallbackRequests { get; private set; }
        public int XlsxRequests { get; private set; }
        public int BrokenXlsxRequests { get; private set; }
        public int XlsxFallbackRequests { get; private set; }
        public Guid AttachmentConversationId { get; set; }
        public Guid AttachmentMessageId { get; } = Guid.NewGuid();
        public Guid AttachmentId { get; } = Guid.NewGuid();
        public Guid PptxAttachmentId { get; } = Guid.NewGuid();
        public const string AttachmentVersion = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        public int AttachmentPreviewRequests { get; private set; }
        public int AttachmentContentRequests { get; private set; }
        public int UnexpectedAttachmentRequests { get; private set; }
        public int PptxAttachmentPreviewRequests { get; private set; }
        public int PptxAttachmentContentRequests { get; private set; }
        public TaskCompletionSource SlowStarted { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public TaskCompletionSource ReleaseSlow { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);

        public WorkspacePreviewFixture()
        {
            _listener.Start(); Address = new Uri($"http://127.0.0.1:{((IPEndPoint)_listener.LocalEndpoint).Port}");
            _server = ServeAsync();
        }

        public (string Title, string FirstPageText, string SecondPageText, int SlideCount) LoadUserPptxSample(string path)
        {
            // Keep the explicitly provided sample in memory; neither the original nor a repository copy is changed.
            // 明确指定的样本仅只读到内存，不修改原件，也不复制进仓库或正式数据目录。
            var source = new FileInfo(path);
            if (!source.Exists || !source.Extension.Equals(".pptx", StringComparison.OrdinalIgnoreCase)
                || source.Length > 64L * 1024 * 1024)
                throw new InvalidDataException("The optional PPTX sample must be an existing presentation within the authorized preview limit.");
            byte[] bytes = File.ReadAllBytes(source.FullName);
            using var archive = new ZipArchive(new MemoryStream(bytes, writable: false), ZipArchiveMode.Read);
            XDocument Part(string name)
            {
                using var stream = (archive.GetEntry(name) ?? throw new InvalidDataException("PPTX sample part missing: " + name)).Open();
                using var reader = System.Xml.XmlReader.Create(stream, new System.Xml.XmlReaderSettings
                {
                    DtdProcessing = System.Xml.DtdProcessing.Prohibit, XmlResolver = null,
                    MaxCharactersInDocument = 16L * 1024 * 1024
                });
                return XDocument.Load(reader);
            }
            XNamespace presentation = "http://schemas.openxmlformats.org/presentationml/2006/main";
            XNamespace drawing = "http://schemas.openxmlformats.org/drawingml/2006/main";
            XNamespace relation = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
            XNamespace package = "http://schemas.openxmlformats.org/package/2006/relationships";
            var relationships = Part("ppt/_rels/presentation.xml.rels").Descendants(package + "Relationship")
                .Where(item => ((string?)item.Attribute("Type"))?.EndsWith("/slide", StringComparison.Ordinal) == true
                    && (string?)item.Attribute("TargetMode") != "External")
                .ToDictionary(item => (string)item.Attribute("Id")!, item => (string)item.Attribute("Target")!);
            var slides = Part("ppt/presentation.xml").Descendants(presentation + "sldId")
                .Select(item => (string?)item.Attribute(relation + "id"))
                .Where(id => id is not null && relationships.ContainsKey(id))
                .Select(id => new Uri(new Uri("https://fixture.invalid/ppt/presentation.xml"), relationships[id!]).AbsolutePath.TrimStart('/'))
                .ToArray();
            if (slides.Length < 2) throw new InvalidDataException("The optional presentation needs at least two slides for navigation validation.");
            string Text(string slide) => Part(slide).Descendants(drawing + "t").Select(item => item.Value.Trim())
                .FirstOrDefault(value => value.Length > 0) ?? throw new InvalidDataException("The optional presentation slide has no text to validate.");
            string first = Text(slides[0]), second = Text(slides[1]);
            _userPptxSample = bytes;
            return (source.Name, first, second, slides.Length);
        }

        private async Task ServeAsync()
        {
            try
            {
                while (!_stop.IsCancellationRequested)
                {
                    var client = await _listener.AcceptTcpClientAsync(_stop.Token);
                    _ = RespondAsync(client);
                }
            }
            catch (OperationCanceledException) { }
        }

        private async Task RespondAsync(TcpClient client)
        {
            using (client)
            {
                try
                {
                    await using var stream = client.GetStream();
                    using var reader = new StreamReader(stream, Encoding.UTF8, false, leaveOpen: true);
                    string[] request = (await reader.ReadLineAsync(_stop.Token) ?? "").Split(' ');
                    int length = 0; bool chunked = false, hasOrigin = false;
                    while (await reader.ReadLineAsync(_stop.Token) is { Length: > 0 } header)
                    {
                        if (header.StartsWith("Content-Length:", StringComparison.OrdinalIgnoreCase)) length = int.Parse(header[15..].Trim());
                        if (header.StartsWith("Transfer-Encoding:", StringComparison.OrdinalIgnoreCase) && header.Contains("chunked", StringComparison.OrdinalIgnoreCase)) chunked = true;
                        if (header.StartsWith("Origin:", StringComparison.OrdinalIgnoreCase)) hasOrigin = true;
                    }
                    char[] body = new char[length];
                    if (length > 0 && await reader.ReadBlockAsync(body.AsMemory(), _stop.Token) != length) throw new IOException("Incomplete fixture request.");
                    string requestBody = new(body);
                    if (chunked)
                    {
                        var chunks = new StringBuilder();
                        while (await reader.ReadLineAsync(_stop.Token) is { } chunkHeader)
                        {
                            int count = Convert.ToInt32(chunkHeader.Split(';')[0], 16);
                            if (count == 0) { await reader.ReadLineAsync(_stop.Token); break; }
                            var chunk = new char[count];
                            if (await reader.ReadBlockAsync(chunk.AsMemory(), _stop.Token) != count) throw new IOException("Incomplete fixture chunk.");
                            chunks.Append(chunk); await reader.ReadLineAsync(_stop.Token);
                        }
                        requestBody = chunks.ToString();
                    }
                    string target = request.Length > 1 ? request[1] : "/";
                    string contentType = "application/json";
                    byte[] bytes;
                    string attachmentEndpoint = $"/api/conversations/{AttachmentConversationId:D}/attachments/{AttachmentId:D}";
                    string pptxAttachmentEndpoint = $"/api/conversations/{AttachmentConversationId:D}/attachments/{PptxAttachmentId:D}";
                    string attachmentQuery = $"messageId={AttachmentMessageId:D}&version={AttachmentVersion}";
                    if (target == attachmentEndpoint + "/preview?" + attachmentQuery + "&offset=0")
                    {
                        AttachmentPreviewRequests++;
                        bytes = JsonSerializer.SerializeToUtf8Bytes(new { kind = "docx", title = "attachment.docx", path = "attachment.docx",
                            version = AttachmentVersion, mimeType = "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                            contentUrl = new Uri(Address, attachmentEndpoint + "/content?" + attachmentQuery).AbsoluteUri });
                    }
                    else if (target == attachmentEndpoint + "/content?" + attachmentQuery)
                    {
                        AttachmentContentRequests++;
                        contentType = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
                        bytes = MinimalDocx();
                    }
                    else if (target == pptxAttachmentEndpoint + "/preview?" + attachmentQuery + "&offset=0")
                    {
                        PptxAttachmentPreviewRequests++;
                        bytes = JsonSerializer.SerializeToUtf8Bytes(new { kind = "pptx", title = "attachment.pptx", path = "attachment.pptx",
                            version = AttachmentVersion, mimeType = "application/vnd.openxmlformats-officedocument.presentationml.presentation",
                            contentUrl = new Uri(Address, pptxAttachmentEndpoint + "/content?" + attachmentQuery).AbsoluteUri });
                    }
                    else if (target == pptxAttachmentEndpoint + "/content?" + attachmentQuery)
                    {
                        PptxAttachmentContentRequests++;
                        contentType = "application/vnd.openxmlformats-officedocument.presentationml.presentation";
                        bytes = MinimalPptx();
                    }
                    else if (target.StartsWith("/api/conversations/", StringComparison.Ordinal))
                    {
                        UnexpectedAttachmentRequests++; bytes = "{}"u8.ToArray();
                    }
                    else if (target == "/agent/workspace/preview")
                    {
                        PreviewRequests++;
                        using var json = JsonDocument.Parse(requestBody);
                        string path = json.RootElement.GetProperty("path").GetString()!;
                        if (path == "user-sample.pptx") UserPptxPreviewRequests++;
                        if (path == "slow.md")
                        {
                            SlowRequests++; SlowStarted.TrySetResult();
                            await ReleaseSlow.Task.WaitAsync(_stop.Token);
                        }
                        LastUserSelected = json.RootElement.GetProperty("userSelected").GetBoolean();
                        string kind = Path.GetExtension(path) switch { ".md" => "markdown", ".mmd" => "mermaid", ".png" or ".svg" => "image", ".pdf" => "pdf", ".docx" => "docx", ".pptx" => "pptx", ".xlsx" => "xlsx", ".html" => "html", _ => "text" };
                        if (path is "broken.docx" or "broken.pptx" or "broken.xlsx" or "late-failure.pptx"
                            && json.RootElement.TryGetProperty("representation", out var representation) && representation.GetString() == "pdf")
                        {
                            kind = "pdf";
                            if (path == "broken.docx") DocxFallbackRequests++;
                            else if (path == "broken.pptx") PptxFallbackRequests++;
                            else if (path == "late-failure.pptx") LatePptxFallbackRequests++;
                            else XlsxFallbackRequests++;
                        }
                        string text = kind == "markdown" ? "# REPORT_FIXTURE\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n$E=mc^2$\n\n```mermaid\nflowchart LR\n A-->B\n```\n\n" + string.Join("\n\n", Enumerable.Range(0, 300).Select(index => $"段落 {index}：用于核对实际文件切换后的阅读位置。"))
                            : kind == "mermaid" ? "flowchart LR\n A[FILE_DIAGRAM]-->B[完成]" : path == "sample.cs" ? "public class Sample { public string Name = \"syntax\"; }" : path == "sample.rs" ? "// 中文说明\npub fn greet() -> String { String::from(\"Hello\") }" : "PLAIN_FILE_FIXTURE\n第二行";
                        if (path == "slow.md") text = "# BACKGROUND_DOCUMENT\n\n```mermaid\nflowchart LR\n A[后台加载]-->B[完成并缓存]\n```";
                        bytes = JsonSerializer.SerializeToUtf8Bytes(new { kind, title = path, path, version = "fixture-v1", text,
                            mimeType = kind switch { "pdf" => "application/pdf", "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                                "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation", "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", _ => "text/plain" }, hasMore = false,
                            contentUrl = kind is "pdf" or "image" or "html" or "docx" or "pptx" or "xlsx" ? "/agent/workspace/content?file=" + (kind == "pdf" ? "sample.pdf" : path) : null as string });
                    }
                    else if (target == "/api/agent/workspace/chrome")
                    {
                        using var json = JsonDocument.Parse(requestBody);
                        string url = json.RootElement.GetProperty("url").GetString()!;
                        await StartChromeAsync(url);
                        string title = await ChromeEvaluateAsync("document.title");
                        var window = await ChromeCommandAsync("Browser.getWindowForTarget", new { targetId = _chromeTabId });
                        using var viewport = JsonDocument.Parse(await ChromeEvaluateAsync("JSON.stringify({width:innerWidth,height:innerHeight,deviceScaleFactor:devicePixelRatio})"));
                        bytes = JsonSerializer.SerializeToUtf8Bytes(new { sessionId = "fixture-native-chrome",
                            tabId = _chromeTabId, processId = _chrome!.Id, url, title,
                            windowId = window.GetProperty("windowId"), windowBounds = window.GetProperty("bounds"), viewport = viewport.RootElement });
                    }
                    else if (target == "/api/agent/workspace/chrome/action")
                    {
                        using var json = JsonDocument.Parse(requestBody);
                        string action = json.RootElement.GetProperty("action").GetString()!;
                        if (action == "close") StopChrome();
                        string url = _chrome?.HasExited == false ? await ChromeEvaluateAsync("location.href") : "";
                        string title = _chrome?.HasExited == false ? await ChromeEvaluateAsync("document.title") : "";
                        using var viewport = JsonDocument.Parse(_chrome?.HasExited == false
                            ? await ChromeEvaluateAsync("JSON.stringify({width:innerWidth,height:innerHeight,deviceScaleFactor:devicePixelRatio})") : "null");
                        bytes = JsonSerializer.SerializeToUtf8Bytes(new { url, title, canGoBack = false, canGoForward = false,
                            viewport = viewport.RootElement });
                    }
                    else if (target.Contains("sample.docx", StringComparison.Ordinal))
                    {
                        DocxRequests++; contentType = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
                        bytes = MinimalDocx(longDocument: true);
                    }
                    else if (target.Contains("broken.docx", StringComparison.Ordinal))
                    {
                        BrokenDocxRequests++; contentType = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
                        bytes = "not a ZIP document"u8.ToArray();
                    }
                    else if (target.Contains("user-sample.pptx", StringComparison.Ordinal))
                    {
                        UserPptxRequests++; contentType = "application/vnd.openxmlformats-officedocument.presentationml.presentation";
                        bytes = _userPptxSample ?? throw new InvalidOperationException("The optional user presentation was not initialized.");
                    }
                    else if (target.Contains("sample.pptx", StringComparison.Ordinal))
                    {
                        PptxRequests++; contentType = "application/vnd.openxmlformats-officedocument.presentationml.presentation";
                        bytes = MinimalPptx();
                    }
                    else if (target.Contains("broken.pptx", StringComparison.Ordinal))
                    {
                        BrokenPptxRequests++; contentType = "application/vnd.openxmlformats-officedocument.presentationml.presentation";
                        bytes = "not a ZIP presentation"u8.ToArray();
                    }
                    else if (target.Contains("late-failure.pptx", StringComparison.Ordinal))
                    {
                        LatePptxRequests++; contentType = "application/vnd.openxmlformats-officedocument.presentationml.presentation";
                        bytes = MinimalPptx(unsupportedSecondPage: true);
                    }
                    else if (target.Contains("sample.xlsx", StringComparison.Ordinal))
                    {
                        XlsxRequests++; contentType = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
                        bytes = MinimalXlsx();
                    }
                    else if (target.Contains("broken.xlsx", StringComparison.Ordinal))
                    {
                        BrokenXlsxRequests++; contentType = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
                        bytes = "not a ZIP workbook"u8.ToArray();
                    }
                    else if (target.StartsWith("/docx-external", StringComparison.Ordinal))
                    {
                        UnexpectedDocxRequests++; contentType = "image/png"; bytes = [];
                    }
                    else if (target.Contains("sample.pdf", StringComparison.Ordinal))
                    {
                        PdfRequests++; contentType = "application/pdf"; bytes = MinimalPdf();
                    }
                    else if (target.Contains("drawing.svg", StringComparison.Ordinal))
                    {
                        contentType = "image/svg+xml";
                        bytes = Encoding.UTF8.GetBytes("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"160\" height=\"80\"><rect width=\"160\" height=\"80\" fill=\"#eceae7\"/><text x=\"12\" y=\"45\">SVG 图形 Preview</text></svg>");
                    }
                    else if (target.Contains("picture.png", StringComparison.Ordinal))
                    {
                        contentType = "image/png";
                        bytes = Convert.FromBase64String("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jFv8AAAAASUVORK5CYII=");
                    }
                    else
                    {
                        contentType = "text/html; charset=utf-8";
                        bytes = Encoding.UTF8.GetBytes("<!doctype html><html><body><h1>" + (target == "/site" ? "BROWSER_SITE_FIXTURE" : "ISOLATED_HTML_FIXTURE")
                            + "</h1><input id=\"native-input\" aria-label=\"Native input\"><button id=\"native-button\" onclick=\"this.dataset.count='1';this.textContent='CLICKED'\">交互图表 Interactive</button><div style=\"height:2400px\">可滚动页面 Scroll fixture</div><script>addEventListener('mousedown',e=>window.lastPointer={x:e.clientX,y:e.clientY,target:e.target.id||e.target.tagName},true)</script></body></html>");
                    }
                    if (hasOrigin) { bytes = "Origin forbidden"u8.ToArray(); contentType = "text/plain"; }
                    string status = hasOrigin ? "403 Forbidden" : "200 OK";
                    string policy = target.Contains("page.html", StringComparison.Ordinal)
                        ? "Content-Security-Policy: sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'none'; form-action 'none'; base-uri 'none'\r\n" : "";
                    byte[] headers = Encoding.ASCII.GetBytes($"HTTP/1.1 {status}\r\nContent-Type: {contentType}\r\nContent-Length: {bytes.Length}\r\n{policy}Connection: close\r\n\r\n");
                    await stream.WriteAsync(headers, _stop.Token); await stream.WriteAsync(bytes, _stop.Token);
                }
                catch (Exception error) when (error is IOException or OperationCanceledException or SocketException) { }
            }
        }

        private async Task StartChromeAsync(string url)
        {
            static string? BundledChrome(string root)
            {
                string manifestPath = Path.Combine(root, "runtime/mcp/manifest.json");
                if (!File.Exists(manifestPath)) return null;
                using var manifest = JsonDocument.Parse(File.ReadAllBytes(manifestPath));
                return Path.Combine(root, manifest.RootElement.GetProperty("browser").GetProperty("path").GetString()!);
            }
            string? bundleRoot = Environment.GetEnvironmentVariable("KYNXA_BROWSER_BUNDLE_ROOT");
            string? chromePath = bundleRoot is null ? null : BundledChrome(bundleRoot);
            for (var directory = new DirectoryInfo(AppContext.BaseDirectory); !File.Exists(chromePath) && directory is not null; directory = directory.Parent)
                chromePath = BundledChrome(Path.Combine(directory.FullName, "artifacts/runtime/native-desktop/win-x64"));
            if (!File.Exists(chromePath)) throw new FileNotFoundException("Prepare the bundled Chromium fixture before this check.");
            string profile = Path.Combine(Path.GetTempPath(), "native-chrome-" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(profile);
            var start = new ProcessStartInfo(chromePath) { UseShellExecute = false, WindowStyle = ProcessWindowStyle.Hidden };
            foreach (string argument in new[] { "--user-data-dir=" + profile, "--remote-debugging-port=0", "--no-first-run",
                "--no-default-browser-check", "--disable-backgrounding-occluded-windows", "--disable-features=CalculateNativeWinOcclusion",
                "--window-position=-10000,-10000", "--new-window", url }) start.ArgumentList.Add(argument);
            _chrome = Process.Start(start) ?? throw new InvalidOperationException("The fixture Chrome process did not start.");
            string portFile = Path.Combine(profile, "DevToolsActivePort");
            DateTime deadline = DateTime.UtcNow.AddSeconds(15);
            while (!File.Exists(portFile) && DateTime.UtcNow < deadline) await Task.Delay(40);
            string port = (await File.ReadAllLinesAsync(portFile, _stop.Token))[0];
            using var http = new HttpClient { Timeout = TimeSpan.FromSeconds(5) };
            while (DateTime.UtcNow < deadline)
            {
                using var targets = JsonDocument.Parse(await http.GetStringAsync("http://127.0.0.1:" + port + "/json/list", _stop.Token));
                foreach (var item in targets.RootElement.EnumerateArray())
                    if (item.TryGetProperty("type", out var type) && type.GetString() == "page"
                        && item.TryGetProperty("url", out var pageUrl) && pageUrl.GetString() == url
                        && item.TryGetProperty("webSocketDebuggerUrl", out var socket))
                    {
                        _chromeWebSocket = socket.GetString(); _chromeTabId = item.GetProperty("id").GetString();
                        return;
                    }
                await Task.Delay(40);
            }
            throw new InvalidOperationException("The fixture Chrome CDP page did not become available.");
        }

        public Task<string> ChromeBodyAsync() => ChromeEvaluateAsync("document.body?.textContent || ''");

        public Task<string> ScrollChromeToTopAsync() => ChromeEvaluateAsync("scrollTo(0,0); ''");

        public uint ChromeProcessId => (uint)(_chrome?.Id ?? 0);

        public async Task<(string TargetId, ChromeWorkspaceWindowBounds Bounds)> CreateChromePopupAsync(int left, int top)
        {
            var created = await ChromeCommandAsync("Target.createTarget", new { url = "about:blank", newWindow = true,
                background = true, left, top, width = 320, height = 160 });
            string targetId = created.GetProperty("targetId").GetString()!;
            var window = await ChromeCommandAsync("Browser.getWindowForTarget", new { targetId });
            var bounds = window.GetProperty("bounds");
            return (targetId, new(bounds.GetProperty("left").GetInt32(), bounds.GetProperty("top").GetInt32(),
                bounds.GetProperty("width").GetInt32(), bounds.GetProperty("height").GetInt32()));
        }

        public Task<JsonElement> CloseChromePopupAsync(string targetId) => ChromeCommandAsync("Target.closeTarget", new { targetId });

        public Task<string> ChromeInteractionStateAsync() => ChromeEvaluateAsync("JSON.stringify({width:innerWidth,height:innerHeight,dpr:devicePixelRatio,scrollY,pointer:window.lastPointer||null,active:document.activeElement?.id||document.activeElement?.tagName,value:document.querySelector('#native-input')?.value,clicked:document.querySelector('#native-button')?.dataset.count||'',input:document.querySelector('#native-input')?.getBoundingClientRect().toJSON(),button:document.querySelector('#native-button')?.getBoundingClientRect().toJSON()})");

        private async Task<string> ChromeEvaluateAsync(string expression)
        {
            var result = await ChromeCommandAsync("Runtime.evaluate", new { expression, returnByValue = true });
            return result.GetProperty("result").GetProperty("value").GetString() ?? "";
        }

        public async Task CaptureChromeAsync(string path)
        {
            var result = await ChromeCommandAsync("Page.captureScreenshot", new { format = "png" });
            await File.WriteAllBytesAsync(path, Convert.FromBase64String(result.GetProperty("data").GetString()!));
        }

        private async Task<JsonElement> ChromeCommandAsync(string method, object parameters)
        {
            using var socket = new ClientWebSocket();
            using var deadline = CancellationTokenSource.CreateLinkedTokenSource(_stop.Token);
            deadline.CancelAfter(TimeSpan.FromSeconds(5));
            await socket.ConnectAsync(new Uri(_chromeWebSocket!), deadline.Token);
            byte[] command = JsonSerializer.SerializeToUtf8Bytes(new { id = 1, method, @params = parameters });
            await socket.SendAsync(command, WebSocketMessageType.Text, true, deadline.Token);
            var buffer = new byte[8192];
            using var response = new MemoryStream();
            while (true)
            {
                var received = await socket.ReceiveAsync(buffer, deadline.Token);
                response.Write(buffer, 0, received.Count);
                if (!received.EndOfMessage) continue;
                using var result = JsonDocument.Parse(response.ToArray());
                if (result.RootElement.TryGetProperty("id", out var id) && id.GetInt32() == 1)
                    return result.RootElement.GetProperty("result").Clone();
                response.SetLength(0);
            }
        }

        private void StopChrome()
        {
            if (_chrome is { HasExited: false }) { _chrome.Kill(entireProcessTree: true); _chrome.WaitForExit(); }
        }

        private static byte[] MinimalPptx(bool unsupportedSecondPage = false)
        {
            using var output = new MemoryStream();
            using (var archive = new ZipArchive(output, ZipArchiveMode.Create, leaveOpen: true))
            {
                void Text(string path, string value)
                {
                    using var stream = archive.CreateEntry(path).Open();
                    stream.Write(Encoding.UTF8.GetBytes(value));
                }
                Text("[Content_Types].xml", """
                    <Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
                      <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
                      <Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/>
                      <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
                      <Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
                      <Override PartName="/ppt/slides/slide2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
                      <Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/>
                      <Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/>
                      <Override PartName="/ppt/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>
                    </Types>
                    """);
                Text("_rels/.rels", """
                    <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdOffice" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>
                    """);
                Text("ppt/presentation.xml", """
                    <p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
                      <p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rIdMaster"/></p:sldMasterIdLst>
                      <p:sldIdLst><p:sldId id="256" r:id="rIdSlide1"/><p:sldId id="257" r:id="rIdSlide2"/></p:sldIdLst>
                      <p:sldSz cx="12192000" cy="6858000" type="screen16x9"/><p:notesSz cx="6858000" cy="9144000"/>
                    </p:presentation>
                    """);
                Text("ppt/_rels/presentation.xml.rels", """
                    <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
                      <Relationship Id="rIdSlide1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
                      <Relationship Id="rIdSlide2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/>
                      <Relationship Id="rIdMaster" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="slideMasters/slideMaster1.xml"/>
                    </Relationships>
                    """);
                const string group = "<p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr>";
                for (int page = 1; page <= 2; page++)
                {
                    string title = page == 1 ? "PPTX_SLIDE_ONE 中文标题" : "PPTX_SLIDE_TWO 第二页";
                    string titlePreset = page == 2 && unsupportedSecondPage ? "star5" : "rect";
                    // A real placeholder may inherit its transform through a layout that has no geometry itself.
                    // 真实占位符允许穿过没有几何信息的版式，继续从母版继承位置与尺寸。
                    string titleTransform = page == 1 ? "" : "<a:xfrm><a:off x=\"457200\" y=\"274320\"/><a:ext cx=\"11277600\" cy=\"914400\"/></a:xfrm>";
                    string inheritedBody = page == 1 ? """
                        <p:sp><p:nvSpPr><p:cNvPr id="5" name="Inherited body"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:spPr/>
                          <p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>PPTX_MASTER_BODY</a:t></a:r></a:p></p:txBody>
                        </p:sp>
                        """ : "";
                    string details = page == 1 ? """
                        <p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="3" name="Fixture table"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr>
                          <p:xfrm><a:off x="457200" y="1645920"/><a:ext cx="5486400" cy="1828800"/></p:xfrm>
                          <a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table"><a:tbl><a:tblPr firstRow="1"/><a:tblGrid><a:gridCol w="2743200"/><a:gridCol w="2743200"/></a:tblGrid>
                            <a:tr h="914400"><a:tc><a:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr lang="en-US" sz="2200"/><a:t>PPTX_TABLE_CELL</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc><a:tc><a:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr sz="2200"/><a:t>42</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc></a:tr>
                          </a:tbl></a:graphicData></a:graphic>
                        </p:graphicFrame>
                        <p:pic><p:nvPicPr><p:cNvPr id="4" name="Embedded fixture image"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr>
                          <p:blipFill><a:blip r:embed="rIdImage"/><a:stretch><a:fillRect/></a:stretch></p:blipFill>
                          <p:spPr><a:xfrm><a:off x="6858000" y="1828800"/><a:ext cx="1828800" cy="914400"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr>
                        </p:pic>
                        """ : "";
                    Text($"ppt/slides/slide{page}.xml", $$"""
                        <p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
                          <p:cSld><p:bg><p:bgPr><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill><a:effectLst/></p:bgPr></p:bg><p:spTree>{{group}}
                            <p:sp><p:nvSpPr><p:cNvPr id="2" name="Fixture title"/><p:cNvSpPr txBox="1"/><p:nvPr><p:ph type="title" idx="0"/></p:nvPr></p:nvSpPr>
                              <p:spPr>{{titleTransform}}<a:prstGeom prst="{{titlePreset}}"><a:avLst/></a:prstGeom></p:spPr>
                              <p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr lang="zh-CN" sz="3200" b="1"/><a:t>{{title}}</a:t></a:r><a:endParaRPr lang="zh-CN"/></a:p></p:txBody>
                            </p:sp>{{details}}{{inheritedBody}}
                          </p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr>
                        </p:sld>
                        """);
                    Text($"ppt/slides/_rels/slide{page}.xml.rels", """
                        <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
                          <Relationship Id="rIdLayout" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/>
                          <Relationship Id="rIdImage" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/picture.png"/>
                        </Relationships>
                        """);
                }
                Text("ppt/slideMasters/slideMaster1.xml", $$"""
                    <p:sldMaster xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
                      <p:cSld><p:spTree>{{group}}
                        <p:sp><p:nvSpPr><p:cNvPr id="2" name="Master title"/><p:cNvSpPr/><p:nvPr><p:ph type="title" idx="0"/></p:nvPr></p:nvSpPr>
                          <p:spPr><a:xfrm><a:off x="457200" y="274320"/><a:ext cx="11277600" cy="914400"/></a:xfrm></p:spPr>
                          <p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:pPr><a:defRPr sz="3200"/></a:pPr></a:p></p:txBody></p:sp>
                        <p:sp><p:nvSpPr><p:cNvPr id="5" name="Master body"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr>
                          <p:spPr><a:xfrm><a:off x="457200" y="4572000"/><a:ext cx="9144000" cy="914400"/></a:xfrm></p:spPr>
                          <p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:pPr><a:defRPr sz="2400"/></a:pPr></a:p></p:txBody></p:sp>
                      </p:spTree></p:cSld><p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/>
                      <p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rIdLayout"/></p:sldLayoutIdLst><p:txStyles><p:titleStyle/><p:bodyStyle/><p:otherStyle/></p:txStyles>
                    </p:sldMaster>
                    """);
                Text("ppt/slideMasters/_rels/slideMaster1.xml.rels", """
                    <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdLayout" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/><Relationship Id="rIdTheme" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme1.xml"/></Relationships>
                    """);
                Text("ppt/slideLayouts/slideLayout1.xml", $$"""
                    <p:sldLayout xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" type="titleAndObject" preserve="1"><p:cSld name="Inherited"><p:spTree>{{group}}
                      <p:sp><p:nvSpPr><p:cNvPr id="2" name="Layout title"/><p:cNvSpPr/><p:nvPr><p:ph type="title" idx="0"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/><a:p/></p:txBody></p:sp>
                      <p:sp><p:nvSpPr><p:cNvPr id="5" name="Layout body"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/><a:p/></p:txBody></p:sp>
                    </p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>
                    """);
                Text("ppt/slideLayouts/_rels/slideLayout1.xml.rels", """
                    <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdMaster" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>
                    """);
                Text("ppt/theme/theme1.xml", """
                    <a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Fixture"><a:themeElements>
                      <a:clrScheme name="Fixture"><a:dk1><a:srgbClr val="181818"/></a:dk1><a:lt1><a:srgbClr val="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="404040"/></a:dk2><a:lt2><a:srgbClr val="F4F4F4"/></a:lt2><a:accent1><a:srgbClr val="7C644B"/></a:accent1><a:accent2><a:srgbClr val="526C56"/></a:accent2><a:accent3><a:srgbClr val="705D7C"/></a:accent3><a:accent4><a:srgbClr val="556C78"/></a:accent4><a:accent5><a:srgbClr val="986B4C"/></a:accent5><a:accent6><a:srgbClr val="74745C"/></a:accent6><a:hlink><a:srgbClr val="6C5645"/></a:hlink><a:folHlink><a:srgbClr val="6C5645"/></a:folHlink></a:clrScheme>
                      <a:fontScheme name="Fixture"><a:majorFont><a:latin typeface="Segoe UI"/><a:ea typeface="Microsoft YaHei UI"/><a:cs typeface=""/></a:majorFont><a:minorFont><a:latin typeface="Segoe UI"/><a:ea typeface="Microsoft YaHei UI"/><a:cs typeface=""/></a:minorFont></a:fontScheme><a:fmtScheme name="Fixture"><a:fillStyleLst/><a:lnStyleLst/><a:effectStyleLst/><a:bgFillStyleLst/></a:fmtScheme>
                    </a:themeElements></a:theme>
                    """);
                using var picture = archive.CreateEntry("ppt/media/picture.png").Open();
                picture.Write(Convert.FromBase64String("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jFv8AAAAASUVORK5CYII="));
            }
            return output.ToArray();
        }

        private static byte[] MinimalXlsx()
        {
            using var output = new MemoryStream();
            using (var archive = new ZipArchive(output, ZipArchiveMode.Create, leaveOpen: true))
            {
                void Text(string path, string value)
                {
                    using var stream = archive.CreateEntry(path).Open();
                    stream.Write(Encoding.UTF8.GetBytes(value));
                }
                Text("[Content_Types].xml", """
                    <Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>
                      <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet3.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
                      <Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/><Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
                    </Types>
                    """);
                Text("_rels/.rels", """
                    <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdOffice" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>
                    """);
                Text("xl/workbook.xml", """
                    <workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><bookViews><workbookView activeTab="0"/></bookViews><sheets><sheet name="数据 Data" sheetId="1" r:id="rIdSheet1"/><sheet name="第二张" sheetId="2" r:id="rIdSheet2"/><sheet name="密集表格" sheetId="3" r:id="rIdSheet3"/></sheets><calcPr calcId="191029" fullCalcOnLoad="0"/></workbook>
                    """);
                Text("xl/_rels/workbook.xml.rels", """
                    <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdSheet1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rIdSheet2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/><Relationship Id="rIdSheet3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet3.xml"/><Relationship Id="rIdStyles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rIdShared" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/></Relationships>
                    """);
                Text("xl/styles.xml", """
                    <styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font><sz val="11"/><name val="Segoe UI"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="1"><border/></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="2"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/><xf numFmtId="14" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/></cellXfs></styleSheet>
                    """);
                Text("xl/sharedStrings.xml", """
                    <sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1"><si><t>XLSX_SHARED_TEXT 中文</t></si></sst>
                    """);
                string rows = string.Join("", Enumerable.Range(3, 118).Select(row => $"<row r=\"{row}\"><c r=\"A{row}\"><v>{row}</v></c><c r=\"L{row}\" t=\"inlineStr\"><is><t>SCROLL_ROW_{row}</t></is></c></row>"));
                Text("xl/worksheets/sheet1.xml", $$"""
                    <worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:L120"/><sheetViews><sheetView workbookViewId="0"/></sheetViews><cols><col min="1" max="12" width="18" customWidth="1"/></cols><sheetData>
                      <row r="1"><c r="A1" t="inlineStr"><is><t>XLSX_TYPED_TITLE 中文</t></is></c><c r="B1"><v>42.5</v></c><c r="C1"><f>SUM(B1,7.5)</f><v>50</v></c><c r="D1" t="s"><v>0</v></c></row>
                      <row r="2"><c r="A2" t="b"><v>1</v></c><c r="B2" s="1"><v>45292</v></c><c r="C2"><f>2+2</f></c><c r="D2" t="inlineStr"><is><t>&lt;img src=x onerror=alert(1)&gt;</t></is></c></row>{{rows}}
                    </sheetData></worksheet>
                    """);
                Text("xl/worksheets/sheet2.xml", """
                    <worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:B2"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>XLSX_SECOND_SHEET 第二张</t></is></c><c r="B1"><v>2026</v></c></row></sheetData></worksheet>
                    """);
                string ColumnName(int column)
                {
                    string name = "";
                    for (; column > 0; column = (column - 1) / 26) name = (char)('A' + (column - 1) % 26) + name;
                    return name;
                }
                var denseRows = new StringBuilder();
                for (int row = 1; row <= 320; row++)
                {
                    denseRows.Append($"<row r=\"{row}\" ht=\"6\" customHeight=\"1\">");
                    for (int column = 1; column <= 128; column++)
                        denseRows.Append($"<c r=\"{ColumnName(column)}{row}\" t=\"inlineStr\"><is><t>DENSE_{row}_{column}</t></is></c>");
                    denseRows.Append("</row>");
                }
                Text("xl/worksheets/sheet3.xml", $$"""
                    <worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:DX320"/><sheetFormatPr defaultRowHeight="6"/><cols><col min="1" max="128" width="1.57" customWidth="1"/></cols><sheetData>{{denseRows}}</sheetData></worksheet>
                    """);
            }
            return output.ToArray();
        }

        private byte[] MinimalDocx(bool longDocument = false)
        {
            using var output = new MemoryStream();
            using (var archive = new ZipArchive(output, ZipArchiveMode.Create, leaveOpen: true))
            {
                void Text(string path, string value)
                {
                    using var stream = archive.CreateEntry(path).Open();
                    stream.Write(Encoding.UTF8.GetBytes(value));
                }
                Text("[Content_Types].xml", """
                    <Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
                      <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
                      <Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/>
                      <Default Extension="html" ContentType="text/html"/>
                      <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
                      <Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
                    </Types>
                    """);
                Text("_rels/.rels", """
                    <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
                      <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
                    </Relationships>
                    """);
                string paragraphs = longDocument ? string.Join("", Enumerable.Range(0, 70)
                    .Select(index => $"<w:p><w:r><w:t>DOCX_SCROLL_ROW_{index} 中文与 English：用于真实滚轮和后台资源完成前的首屏交互检查。</w:t></w:r></w:p>")) : "";
                Text("word/document.xml", $$"""
                    <w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
                      xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture">
                      <w:body>
                        <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>DOCX_BROWSER_FIXTURE 中文标题</w:t></w:r></w:p>
                        <w:p><w:r><w:t>正文包含中文与 English，保留文档格式。</w:t></w:r></w:p>
                        <w:tbl><w:tblPr><w:tblBorders><w:top w:val="single" w:sz="4"/><w:left w:val="single" w:sz="4"/><w:bottom w:val="single" w:sz="4"/><w:right w:val="single" w:sz="4"/><w:insideH w:val="single" w:sz="4"/><w:insideV w:val="single" w:sz="4"/></w:tblBorders></w:tblPr>
                          <w:tblGrid><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/></w:tblGrid><w:tr>
                            <w:tc><w:p><w:r><w:t>DOCX_TABLE_CELL</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>42</w:t></w:r></w:p></w:tc>
                          </w:tr></w:tbl>
                        <w:p><w:r><w:drawing><wp:inline><wp:extent cx="914400" cy="457200"/><wp:docPr id="1" name="Embedded image"/>
                          <a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic>
                            <pic:nvPicPr><pic:cNvPr id="1" name="picture.png"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="rIdImage"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>
                            <pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="457200"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr>
                          </pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>
                        <w:hyperlink r:id="rIdUnsafeLink"><w:r><w:t>Unsafe link</w:t></w:r></w:hyperlink>
                        <w:altChunk r:id="rIdAltChunk"/>
                        {{paragraphs}}
                        <w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="720" w:right="720" w:bottom="720" w:left="720"/></w:sectPr>
                      </w:body>
                    </w:document>
                    """);
                Text("word/styles.xml", """
                    <w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                      <w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:rPr><w:b/><w:sz w:val="36"/></w:rPr></w:style>
                    </w:styles>
                    """);
                Text("word/_rels/document.xml.rels", $$"""
                    <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
                      <Relationship Id="rIdStyles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
                      <Relationship Id="rIdImage" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/picture.png"/>
                      <Relationship Id="rIdExternal" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="{{new Uri(Address, "/docx-external/image.png").AbsoluteUri}}" TargetMode="External"/>
                      <Relationship Id="rIdUnsafeLink" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="javascript:parent.__unsafeDocx=1" TargetMode="External"/>
                      <Relationship Id="rIdAltChunk" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/aFChunk" Target="alt.html"/>
                    </Relationships>
                    """);
                Text("word/alt.html", "<script>parent.__unsafeDocx=1</script><p>ALT_CHUNK_EXECUTED</p>");
                using var picture = archive.CreateEntry("word/media/picture.png").Open();
                picture.Write(Convert.FromBase64String("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jFv8AAAAASUVORK5CYII="));
            }
            return output.ToArray();
        }

        private static byte[] MinimalPdf()
        {
            const string pageContent = "BT /F1 18 Tf 20 240 Td (PDF FIXTURE) Tj ET\n";
            string[] objects = ["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>", $"<< /Length {pageContent.Length} >>\nstream\n{pageContent}endstream"];
            var text = new StringBuilder("%PDF-1.4\n"); var offsets = new List<int> { 0 };
            for (int index = 0; index < objects.Length; index++) { offsets.Add(text.Length); text.Append($"{index + 1} 0 obj\n{objects[index]}\nendobj\n"); }
            int xref = text.Length; text.Append("xref\n0 6\n0000000000 65535 f \n");
            foreach (int offset in offsets.Skip(1)) text.Append(offset.ToString("D10") + " 00000 n \n");
            text.Append($"trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n");
            return Encoding.ASCII.GetBytes(text.ToString());
        }

        public async ValueTask DisposeAsync()
        {
            StopChrome(); _chrome?.Dispose();
            _stop.Cancel(); _listener.Stop();
            await _server; _stop.Dispose();
        }
    }
}

namespace KYNXA_Desktop.Services
{
    // Keep this rendering harness independent of product startup and user model settings.
    // 展示夹具独立于正式启动流程及用户模型配置。
    public static class ModelGatewayService
    {
        public static Uri FixtureAddress { get; set; } = new("http://127.0.0.1:1");
        public static Uri Address => FixtureAddress;
        public static Task EnsureReadyAsync(CancellationToken cancellationToken = default) => Task.CompletedTask;
    }
}
