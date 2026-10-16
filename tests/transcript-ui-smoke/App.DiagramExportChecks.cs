using System.Text;
using System.Text.Json;
using System.Xml.Linq;
using System.Globalization;
using System.Runtime.InteropServices.WindowsRuntime;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Windows.Graphics.Imaging;
using Windows.Storage;

namespace TranscriptUiSmoke;

public partial class App
{
    // Keep the production bridge, validation and file writer; replace only the interactive picker.
    // 保留正式桥接、校验及文件写入，仅替换需要人工操作的保存选择器。
    private async Task CheckDiagramExportAsync()
    {
        string language = UiText.Language, paletteId = AppearanceService.Current.Id, mode = AppearanceService.Mode;
        string? accent = AppearanceService.CustomAccent;
        double fontSize = AppearanceService.ChatFontSize;
        var size = _window!.AppWindow.Size;
        void RecordExportPayload(Microsoft.Web.WebView2.Core.CoreWebView2 sender, Microsoft.Web.WebView2.Core.CoreWebView2WebMessageReceivedEventArgs args)
        {
            // Diagnostics contain only these isolated synthetic fixtures, never user conversations.
            // 诊断只包含本测试的隔离样例，不读取用户正式会话。
            using var message = JsonDocument.Parse(args.WebMessageAsJson);
            if (message.RootElement.TryGetProperty("type", out var type) && type.GetString() == "exportDiagram")
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "diagram-export-last-payload.json"), args.WebMessageAsJson);
        }
        _transcript.Browser.CoreWebView2.WebMessageReceived += RecordExportPayload;
        try
        {
            UiText.Initialize("zh-CN");
            AppearanceService.ApplyPreferences(AppearanceService.DefaultPaletteId, "light", 15, null);
            _transcript.ClearSelection();
            await CheckExportTypesAsync();
            await CheckAutomaticExportAsync();
            await CheckStandaloneExportAsync();
            await CheckExportReceiptsAsync();
            await CheckExportNavigationAsync();
            await CheckExportPreparationAsync();
            await CheckExportSafetyAsync();
            await CheckExportBoundsAsync();
            await CheckExportLayoutAsync();
        }
        finally
        {
            _transcript.DiagramExportSaveOverride = null;
            _transcript.Browser.CoreWebView2.WebMessageReceived -= RecordExportPayload;
            await EvalAsync<bool>("(() => { window.kynxaDiagrams.closeViewer(); getSelection().removeAllRanges(); return true; })()");
            UiText.Initialize(language);
            AppearanceService.ApplyPreferences(paletteId, mode, fontSize, accent);
            _window.AppWindow.Resize(size);
        }
    }

    private static readonly (string Type, string Source, string Label)[] ExportFixtures =
    [
        ("flowchart", "flowchart LR\n A[\"开始 Start<br/>第二行 Line two\"] --> B{\"检查参数 Check\"}\n B -->|成功 Yes| C[\"`完整导出中文与English长标签\nThe whole diagram remains readable after zooming and panning without clipping`\"]\n B -->|失败 No| D[\"重试 Retry\"]", "开始"),
        ("graph", "graph LR\n A[需求 Requirements] --> B[设计 Design] --> C[完成 Finish]", "需求"),
        ("sequenceDiagram", "sequenceDiagram\n participant U as 用户 User\n participant G as 网关 Gateway\n U->>G: 发送 Send\n G-->>U: 回复 Reply\n Note over U,G: 中文说明<br/>English note", "用户"),
        ("stateDiagram-v2", "stateDiagram-v2\n [*] --> Waiting\n state \"等待 Waiting\" as Waiting\n state \"完成 Done\" as Done\n Waiting --> Done: 启动 Start\n Done --> [*]", "等待"),
        ("classDiagram", "classDiagram\n class Task {\n +String name\n +run()\n }\n class Agent\n Agent --> Task : 执行 Execute", "Task"),
        ("erDiagram", "erDiagram\n USER ||--o{ MESSAGE : 发送\n USER {\n int id PK\n string name\n }\n MESSAGE {\n int id PK\n string content\n }", "USER"),
        ("gantt", "gantt\n title 项目进度 Schedule\n dateFormat YYYY-MM-DD\n section 开发 Development\n 设计 Design :done, d, 2026-10-01, 2d\n 验收 Verify :after d, 2d", "项目进度"),
        ("mindmap", "mindmap\n root((课设 Project))\n  前端 UI\n   主题 Theme\n  测试 Tests\n   导出 Export", "课设")
    ];

    private async Task<DiagramExportDocument> ExportFileAsync(ConversationMessageViewModel row, string format, string fileName, bool fullscreen = false)
    {
        var received = new TaskCompletionSource<DiagramExportDocument>(TaskCreationOptions.RunContinuationsAsynchronously);
        _transcript.DiagramExportSaveOverride = async document =>
        {
            try
            {
                var folder = await StorageFolder.GetFolderFromPathAsync(Path.GetTempPath());
                var file = await folder.CreateFileAsync(fileName, CreationCollisionOption.ReplaceExisting);
                await DiagramExport.WriteAsync(file, document);
                received.TrySetResult(document);
                return DiagramExportOutcome.Success;
            }
            catch (Exception error)
            {
                received.TrySetException(error);
                throw;
            }
        };
        string container = fullscreen ? "document.querySelector('.mermaid-viewer')" : DiagramBlockExpression(row);
        Check(await EvalAsync<bool>($"(() => {{ const b={container}?.querySelector('[data-diagram-action=export-{format}]'); if (!b || b.disabled) return false; b.click(); return true; }})()"),
            format + " export is independently actionable in its diagram toolbar");
        DiagramExportDocument document;
        try { document = await received.Task.WaitAsync(TimeSpan.FromSeconds(20)); }
        catch (TimeoutException error)
        {
            // Preserve the specific native validation cause when no writer was reached.
            // 未进入保存阶段时重新验证该隔离测试载荷，保留具体原因而不是只报告等待超时。
            string payloadPath = Path.Combine(Path.GetTempPath(), "diagram-export-last-payload.json");
            if (File.Exists(payloadPath))
            {
                using var payload = JsonDocument.Parse(await File.ReadAllTextAsync(payloadPath));
                var data = payload.RootElement;
                try { await DiagramExport.ValidateAsync(data.GetProperty("format").GetString()!, data.GetProperty("content").GetString()!); }
                catch (Exception validation) { throw new InvalidOperationException(fileName + " was rejected before native writing", validation); }
            }
            throw new InvalidOperationException(fileName + " did not reach the native writer", error);
        }
        await WaitAsync(container + "?.querySelector('.mermaid-export-status')?.dataset.exportState === 'success'", format + " waits for the actual native file write before success feedback");
        byte[] savedBytes = await File.ReadAllBytesAsync(Path.Combine(Path.GetTempPath(), fileName));
        Check(document.Format == format && document.Bytes.SequenceEqual(savedBytes), format + " file bytes equal the immutable validated document");
        return document;
    }

    private async Task CheckExportTypesAsync()
    {
        var chat = Guid.NewGuid();
        var rows = ExportFixtures.Select(fixture => Message(chat, "assistant", MermaidFence(fixture.Source))).ToArray();
        string formal = JsonSerializer.Serialize(rows.Select(row => row.Message));
        _transcript.ShowConversation(chat, rows);
        await WaitAsync(string.Join(" && ", rows.Select(row => DiagramBlockExpression(row) + "?.dataset.diagramState === 'ready'")), "all seven diagram families and graph alias are ready for export");
        for (int index = 0; index < rows.Length; index++)
        {
            var fixture = ExportFixtures[index];
            if (fixture.Type == "mindmap")
            {
                Check(await EvalAsync<bool>($"(() => {{ const b={DiagramBlockExpression(rows[index])}; const node=b.querySelector('.mindmap-node.section-root'); const circle=node?.querySelector('circle'); const text=[...node?.querySelectorAll('text')||[]].find(t=>t.textContent.includes('Project')); if(!circle||!text)return false; const c=circle.getBoundingClientRect(),t=text.getBoundingClientRect(); return Math.abs((c.left+c.right-t.left-t.right)/2)<=1 && getComputedStyle(text).textAnchor==='middle'; }})()"),
                    "mindmap circular root label is centered before export instead of beginning at its circle center");
            }
            var svg = await ExportFileAsync(rows[index], "svg", "diagram-export-" + fixture.Type + ".svg");
            var xml = XDocument.Parse(Encoding.UTF8.GetString(svg.Bytes));
            var root = xml.Root!;
            Check(root.Name == XName.Get("svg", "http://www.w3.org/2000/svg") && root.Value.Contains(fixture.Label, StringComparison.Ordinal), fixture.Type + " SVG retains actual bilingual text in a standalone SVG document");
            Check(root.Attribute("width") is not null && root.Attribute("height") is not null && root.Attribute("viewBox") is not null, fixture.Type + " SVG has explicit full image dimensions");
            Check(!root.Descendants().Any(element => element.Name.LocalName is "script" or "foreignObject" or "image" or "animate" or "set" or "style"), fixture.Type + " export contains only inert geometry and inlined presentation");
            Check(root.Descendants().Where(element => element.Name.LocalName is "text" or "tspan").Any(element => element.Attribute("style") is not null || element.Attribute("font-family") is not null), fixture.Type + " export includes standalone text styling");
            var png = await ExportFileAsync(rows[index], "png", "diagram-export-" + fixture.Type + ".png");
            using var stream = await (await StorageFile.GetFileFromPathAsync(Path.Combine(Path.GetTempPath(), "diagram-export-" + fixture.Type + ".png"))).OpenReadAsync();
            var decoder = await BitmapDecoder.CreateAsync(stream);
            Check(decoder.DecoderInformation.CodecId == BitmapDecoder.PngDecoderId && decoder.PixelWidth > 0 && decoder.PixelHeight > 0, fixture.Type + " output is a decodable real PNG");
            Check(decoder.PixelWidth <= 8192 && decoder.PixelHeight <= 8192 && (long)decoder.PixelWidth * decoder.PixelHeight <= 16000000, fixture.Type + " PNG remains within allocation limits");
            var pixels = (await decoder.GetPixelDataAsync()).DetachPixelData();
            Check(pixels.Distinct().Take(12).Count() > 8, fixture.Type + " PNG contains rendered geometry and text instead of a blank rectangle");
            Check(png.Width == decoder.PixelWidth && png.Height == decoder.PixelHeight, fixture.Type + " validated PNG dimensions match the saved image");
            double[] viewBox = root.Attribute("viewBox")!.Value.Split([' ', ','], StringSplitOptions.RemoveEmptyEntries).Select(value => double.Parse(value, CultureInfo.InvariantCulture)).ToArray();
            // Integer raster dimensions can differ by one pixel on the short axis of a wide/tall diagram.
            // 很宽或很高的图在短边取整时允许一像素误差，不能将它放大成虚假的长边误差。
            double aspectRounding = Math.Abs(decoder.PixelWidth * viewBox[3] - decoder.PixelHeight * viewBox[2]) / Math.Max(viewBox[2], viewBox[3]);
            Check(aspectRounding <= 1,
                fixture.Type + " PNG aspect ratio includes the whole SVG viewBox");
        }
        var firstSvg = await ExportFileAsync(rows[0], "svg", "diagram-export-before-zoom.svg");
        await EvalAsync<bool>($"(() => {{ const b={DiagramBlockExpression(rows[0])}; b.querySelector('[data-diagram-action=zoom-in]').click(); b.querySelector('.mermaid-viewport').dispatchEvent(new KeyboardEvent('keydown', {{key:'ArrowLeft',bubbles:true}})); return true; }})()");
        var zoomedSvg = await ExportFileAsync(rows[0], "svg", "diagram-export-after-zoom.svg");
        Check(EquivalentDiagramExports(firstSvg, zoomedSvg), "exported geometry and full viewBox are unaffected by current zoom and pan");
        await EvalAsync<bool>($"(() => {{ {DiagramBlockExpression(rows[0])}.querySelector('[data-diagram-action=fullscreen]').click(); return true; }})()");
        var fullscreenSvg = await ExportFileAsync(rows[0], "svg", "diagram-export-fullscreen.svg", fullscreen: true);
        Check(EquivalentDiagramExports(firstSvg, fullscreenSvg), "fullscreen exports the same original full diagram, not the viewer transform");
        await EvalAsync<bool>("(() => { kynxaDiagrams.closeViewer(); return true; })()");
        Check(formal == JsonSerializer.Serialize(rows.Select(row => row.Message)), "export never changes formal messages, source or stream metadata");
        _metrics["diagramExportTypes"] = ExportFixtures.Select(fixture => fixture.Type).ToArray();
    }

    private static bool EquivalentDiagramExports(DiagramExportDocument first, DiagramExportDocument second)
    {
        // IDs may be remapped for safe standalone references; compare the complete normalized document.
        // 独立安全引用允许重新分配 ID；比较完整规范化文档，不能只比较文字或截图。
        static string Normalize(byte[] bytes)
        {
            var xml = XDocument.Parse(Encoding.UTF8.GetString(bytes));
            var attributes = xml.Root!.DescendantsAndSelf().SelectMany(element => element.Attributes()).ToArray();
            var ids = attributes.Where(attribute => attribute.Name.LocalName == "id").Select((attribute, index) => (Old: attribute.Value, New: "normalized-id-" + index)).ToArray();
            foreach (var attribute in attributes)
            {
                foreach (var id in ids)
                {
                    if (attribute.Name.LocalName == "id" && attribute.Value == id.Old) attribute.Value = id.New;
                    else attribute.Value = attribute.Value.Replace("url(#" + id.Old + ")", "url(#" + id.New + ")", StringComparison.Ordinal);
                }
            }
            return xml.ToString(SaveOptions.DisableFormatting);
        }
        return Normalize(first.Bytes) == Normalize(second.Bytes);
    }

    private void CheckAutomaticPngSize(DiagramExportDocument png, double width, double height, string description)
    {
        // Rasterization uses the full intrinsic bounds and the largest scale allowed by both limits.
        // 栅格化使用完整固有边界，自动取同时满足边长和总像素限制的最大倍率。
        double scale = Math.Min(Math.Min(8192 / width, 8192 / height), Math.Sqrt(16000000 / (width * height)));
        uint expectedWidth = (uint)Math.Max(1, Math.Floor(width * scale));
        uint expectedHeight = (uint)Math.Max(1, Math.Floor(height * scale));
        Check(png.Width == expectedWidth && png.Height == expectedHeight && png.Width <= 8192 && png.Height <= 8192
            && (long)png.Width * png.Height <= 16000000, description);
    }

    private async Task CheckOpaqueWhitePngAsync(string fileName, string description)
    {
        using var stream = await (await StorageFile.GetFileFromPathAsync(Path.Combine(Path.GetTempPath(), fileName))).OpenReadAsync();
        var decoder = await BitmapDecoder.CreateAsync(stream);
        byte[] pixels = (await decoder.GetPixelDataAsync(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Straight,
            new BitmapTransform(), ExifOrientationMode.IgnoreExifOrientation, ColorManagementMode.DoNotColorManage)).DetachPixelData();
        // Corner samples verify the white backdrop; inspect every alpha byte to reject hidden transparency.
        // 四角采样验证白色背景，同时扫描全部 alpha 字节，避免遗漏图形内部的透明区域。
        bool opaque = true;
        for (int offset = 3; offset < pixels.Length; offset += 4)
        {
            if (pixels[offset] == 255) continue;
            opaque = false;
            break;
        }
        int width = checked((int)decoder.PixelWidth), height = checked((int)decoder.PixelHeight);
        int[] corners = [0, (width - 1) * 4, (height - 1) * width * 4, (width * height - 1) * 4];
        Check(opaque && corners.All(offset => pixels[offset] == 255 && pixels[offset + 1] == 255 && pixels[offset + 2] == 255), description);
    }

    private async Task CheckAutomaticExportAsync()
    {
        var chat = Guid.NewGuid();
        var row = Message(chat, "assistant", "AUTOMATIC_SELECTION\n\n" + MermaidFence("flowchart LR\n A[显式样式 Explicit style] --> B[默认文本 Default text]\n B --> C[类样式 Class style]\n style A fill:#173a5e,color:#ffffff\n classDef emphasis fill:#f4b860,color:#173a5e\n class C emphasis"));
        string formal = JsonSerializer.Serialize(row.Message);
        _transcript.ShowConversation(chat, [row]);
        await WaitAsync(DiagramBlockExpression(row) + "?.dataset.diagramState==='ready'", "automatic white export fixture is ready");
        Check(await EvalAsync<bool>("(() => {const group=document.querySelector('.mermaid-block .mermaid-export-controls');return group?.querySelectorAll('[data-diagram-action^=export-]').length===2 && !group.querySelector('select,input,.mermaid-export-option,[data-diagram-export-scale],[data-diagram-export-transparent]');})()"),
            "inline export toolbar contains SVG/PNG actions without scale or transparency controls");
        Check(await EvalAsync<bool>("""
            (() => {
              const nodes=[...document.querySelectorAll('.mermaid-viewport svg .node')];
              const node=label=>nodes.find(element=>element.textContent.includes(label));
              const shape=label=>getComputedStyle(node(label).querySelector('rect'));
              const text=label=>getComputedStyle(node(label).querySelector('text,tspan')).fill;
              return shape('Explicit style').fill==='rgb(23, 58, 94)' && text('Explicit style')==='rgb(255, 255, 255)'
                && shape('Class style').fill==='rgb(244, 184, 96)' && text('Class style')==='rgb(23, 58, 94)'
                && text('Default text')==='rgb(36, 39, 45)';
            })()
            """), "explicit Mermaid style and classDef colors survive while default labels use dark text");
        var naturalSvg = await ExportFileAsync(row, "svg", "diagram-export-automatic-natural.svg");
        var xml = XDocument.Parse(Encoding.UTF8.GetString(naturalSvg.Bytes));
        double[] box = xml.Root!.Attribute("viewBox")!.Value.Split([' ', ','], StringSplitOptions.RemoveEmptyEntries)
            .Select(value => double.Parse(value, CultureInfo.InvariantCulture)).ToArray();
        Check(xml.Root.Elements().First().Name.LocalName == "rect" && xml.Root.Elements().First().Attribute("fill")?.Value == "#ffffff"
            && xml.Root.Attribute("style")!.Value.Contains("background-color: #ffffff;", StringComparison.Ordinal),
            "standalone SVG paints a white full-viewBox backdrop and white viewport letterboxing");
        string exportedSvg = Encoding.UTF8.GetString(naturalSvg.Bytes);
        Check(exportedSvg.Contains("fill: rgb(23, 58, 94);", StringComparison.Ordinal)
            && exportedSvg.Contains("fill: rgb(244, 184, 96);", StringComparison.Ordinal)
            && xml.Root.Descendants().Any(element => (element.Name.LocalName is "text" or "tspan")
                && element.Value.Contains("Explicit style", StringComparison.Ordinal) && element.Attribute("style")!.Value.Contains("fill: rgb(255, 255, 255);", StringComparison.Ordinal))
            && xml.Root.Descendants().Any(element => (element.Name.LocalName is "text" or "tspan")
                && element.Value.Contains("Class style", StringComparison.Ordinal) && element.Attribute("style")!.Value.Contains("fill: rgb(23, 58, 94);", StringComparison.Ordinal))
            && xml.Root.Descendants().Any(element => (element.Name.LocalName is "text" or "tspan")
                && element.Value.Contains("Default text", StringComparison.Ordinal) && element.Attribute("style")!.Value.Contains("fill: rgb(36, 39, 45);", StringComparison.Ordinal)),
            "standalone SVG freezes explicit source colors and readable default text into its own styles");
        var automaticPng = await ExportFileAsync(row, "png", "diagram-export-automatic.png");
        CheckAutomaticPngSize(automaticPng, box[2], box[3], "PNG automatically uses the largest complete aspect-preserving image within both allocation limits");
        await CheckOpaqueWhitePngAsync("diagram-export-automatic.png", "automatic PNG has white corner pixels and opaque alpha across the actual decoded image");
        await EvalAsync<bool>("(() => { document.querySelector('[data-diagram-action=fullscreen]').click();return true; })()");
        await WaitAsync("!!document.querySelector('.mermaid-viewer[open]')", "automatic export actions reach the actual fullscreen viewer");
        Check(await EvalAsync<bool>("(() => {const group=document.querySelector('.mermaid-viewer .mermaid-export-controls');return group?.querySelectorAll('[data-diagram-action^=export-]').length===2 && !group.querySelector('select,input,.mermaid-export-option,[data-diagram-export-scale],[data-diagram-export-transparent]');})()"),
            "fullscreen export toolbar contains SVG/PNG actions without scale or transparency controls");
        var fullscreen = await ExportFileAsync(row, "png", "diagram-export-automatic-fullscreen.png", fullscreen: true);
        CheckAutomaticPngSize(fullscreen, box[2], box[3], "fullscreen PNG uses the same automatic full-diagram dimensions independently of viewer zoom");
        Check(fullscreen.Bytes.SequenceEqual(automaticPng.Bytes), "inline and fullscreen PNG exports contain the same complete raster image");
        await CheckOpaqueWhitePngAsync("diagram-export-automatic-fullscreen.png", "fullscreen PNG retains the same opaque white document background");
        await EvalAsync<bool>("(() => { kynxaDiagrams.closeViewer();return true; })()");
        await EvalAsync<bool>("(() => { const node=document.querySelector('.message-body > p').firstChild;const range=document.createRange();range.setStart(node,0);range.setEnd(node,7);getSelection().removeAllRanges();getSelection().addRange(range);window.__automaticSelection=getSelection().toString();return true; })()");
        await ExportFileAsync(row, "svg", "diagram-export-automatic-selection.svg");
        Check(await EvalAsync<bool>("getSelection().toString()===__automaticSelection"), "automatic export preserves the existing reading selection");
        await EvalAsync<bool>("(() => { getSelection().removeAllRanges();return true; })()");
        Check(formal == JsonSerializer.Serialize(row.Message), "automatic white export never changes formal messages or explicit Mermaid source styles");
        _metrics["diagramAutomaticExport"] = new { maximumSidePx = 8192, maximumPixels = 16000000,
            pngWidth = automaticPng.Width, pngHeight = automaticPng.Height, background = "#ffffff",
            scope = "actual inline/fullscreen SVG/PNG through native validation and writing; decoded opaque white pixels and explicit source styles" };
    }

    private async Task CheckStandaloneExportAsync()
    {
        string[] names = Enumerable.Range(0, 24).Select(index => DiagramExport.CreateSuggestedFileName(index % 2 == 0 ? "svg" : "png")).ToArray();
        Check(names.Distinct(StringComparer.OrdinalIgnoreCase).Count() == names.Length
            && names.All(name => name.StartsWith("KYNXA-diagram-", StringComparison.Ordinal) && name.IndexOfAny(Path.GetInvalidFileNameChars()) < 0
                && !name.Contains(' ') && Path.GetFileName(name) == name),
            "rapid SVG and PNG exports receive distinct safe suggested filenames without any conversation text");
        Check(names[0].StartsWith("KYNXA-diagram-svg-", StringComparison.Ordinal) && names[1].StartsWith("KYNXA-diagram-png-", StringComparison.Ordinal),
            "suggested filenames identify the export format and retain timestamp plus sequence");
        var chat = Guid.NewGuid();
        string source = "flowchart LR\n" + string.Join(" --> ", Enumerable.Range(0, 8).Select(index => "N" + index + "[步骤 Step " + index + "]"));
        var row = Message(chat, "assistant", MermaidFence(source));
        _transcript.ShowConversation(chat, [row]);
        await WaitAsync(DiagramBlockExpression(row) + "?.dataset.diagramState==='ready'", "a genuinely wide standalone SVG fixture renders");
        var svg = await ExportFileAsync(row, "svg", "diagram-export-responsive-wide.svg");
        var root = XDocument.Parse(Encoding.UTF8.GetString(svg.Bytes)).Root!;
        Check(svg.Width > svg.Height * 5 && root.Value.Contains("Step 0", StringComparison.Ordinal) && root.Value.Contains("Step 7", StringComparison.Ordinal),
            "saved wide SVG includes both ends of the entire diagram and natural geometry dimensions");
        string style = root.Attribute("style")!.Value;
        Check(style.Contains("width: 100%;", StringComparison.Ordinal) && style.Contains("height: 100vh;", StringComparison.Ordinal)
            && root.Attribute("preserveAspectRatio")!.Value == "xMidYMid meet",
            "standalone SVG has fixed viewport fit rules and centered aspect-preserving full-graph scaling");
        var png = await ExportFileAsync(row, "png", "diagram-export-responsive-wide.png");
        double[] box = root.Attribute("viewBox")!.Value.Split([' ', ','], StringSplitOptions.RemoveEmptyEntries)
            .Select(value => double.Parse(value, CultureInfo.InvariantCulture)).ToArray();
        CheckAutomaticPngSize(png, box[2], box[3],
            "standalone responsive SVG rules preserve the PNG decoder's automatic maximum intrinsic raster dimensions");
        _metrics["standaloneWideSvgPath"] = Path.Combine(Path.GetTempPath(), "diagram-export-responsive-wide.svg");
    }

    private async Task CheckExportReceiptsAsync()
    {
        UiText.Initialize("en");
        var chat = Guid.NewGuid();
        var row = Message(chat, "assistant", MermaidFence("flowchart LR\n A[成功 Success] --> B[失败 Retry]"));
        _transcript.ShowConversation(chat, [row]);
        await WaitAsync(DiagramBlockExpression(row) + "?.dataset.diagramState === 'ready'", "receipt fixture renders");
        await WaitAsync("document.querySelector('[data-diagram-action=export-svg]').getAttribute('aria-label')==='Export SVG'", "native English export labels reach the active graph");
        foreach (var outcome in new[] { DiagramExportOutcome.Cancelled, DiagramExportOutcome.Error })
        {
            _transcript.DiagramExportSaveOverride = _ => Task.FromResult(outcome);
            await EvalAsync<bool>("(() => { document.querySelector('[data-diagram-action=export-svg]').click(); return true; })()");
            string expected = outcome == DiagramExportOutcome.Cancelled ? "cancelled" : "error";
            await WaitAsync("document.querySelector('.mermaid-export-status')?.dataset.exportState === '" + expected + "'", expected + " uses local feedback and re-enables retry");
            string expectedText = outcome == DiagramExportOutcome.Cancelled ? UiText.Get("已取消导出") : UiText.Get("导出失败，请重试。");
            Check(await EvalAsync<string>("document.querySelector('.mermaid-export-status').textContent") == expectedText, expected + " visible feedback uses the active English translation");
            Check(await EvalAsync<bool>("!document.querySelector('[data-diagram-action=export-svg]').disabled && !document.querySelector('[data-diagram-action=export-png]').disabled"), expected + " leaves both export actions usable");
        }
        _transcript.DiagramExportSaveOverride = _ => throw new IOException("isolated write failure");
        await EvalAsync<bool>("(() => { document.querySelector('[data-diagram-action=export-svg]').click(); return true; })()");
        await WaitAsync("document.querySelector('.mermaid-export-status')?.dataset.exportState === 'error'", "a thrown native write error is acknowledged locally");
        await ExportFileAsync(row, "svg", "diagram-export-retry.svg");
        var second = Message(chat, "assistant", MermaidFence("flowchart TD\n C[Second] --> D[Independent]"));
        _transcript.ShowConversation(chat, [row, second]);
        await WaitAsync(DiagramBlockExpression(second) + "?.dataset.diagramState === 'ready'", "a second graph is available during export concurrency checks");
        var release = new TaskCompletionSource<DiagramExportOutcome>(TaskCreationOptions.RunContinuationsAsynchronously);
        int calls = 0;
        _transcript.DiagramExportSaveOverride = _ => { calls++; return release.Task; };
        await EvalAsync<bool>("(() => { const b=document.querySelector('[data-diagram-action=export-svg]'); b.click(); b.click(); document.querySelector('[data-diagram-action=export-png]').click(); return true; })()");
        await WaitAsync("document.querySelector('.mermaid-export-status')?.dataset.exportState === 'saving'", "export waits visibly for the native picker result");
        Check(await EvalAsync<string>("document.querySelector('.mermaid-export-status').textContent") == UiText.Get("正在保存图表…"), "pending native saving uses visible English feedback");
        Check(calls == 1 && await EvalAsync<bool>("[...document.querySelectorAll('[data-diagram-action^=export-]')].every(button=>button.disabled)"), "repeated clicks and other graphs cannot allocate concurrent export canvases");
        await PostExportProbeAsync(chat, second, "svg", Convert.ToBase64String(Encoding.UTF8.GetBytes("<svg xmlns='http://www.w3.org/2000/svg' width='100' height='100' viewBox='0 0 100 100'><rect width='100' height='100'/></svg>")));
        Check(calls == 1, "native also rejects a valid second graph request while the first picker is pending");
        _transcript.ShowConversation(chat, [row, second, Message(chat, "user", "SAME_CHAT_UPDATE")]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('SAME_CHAT_UPDATE')", "same-chat updates continue while a save is pending");
        release.SetResult(DiagramExportOutcome.Cancelled);
        await WaitAsync("document.querySelector('.mermaid-export-status')?.dataset.exportState === 'cancelled'", "cancelling a delayed picker releases the busy state");
    }

    private async Task CheckExportNavigationAsync()
    {
        var firstChat = Guid.NewGuid();
        var first = Message(firstChat, "assistant", MermaidFence("flowchart LR\n A[SNAPSHOT_A 中文] --> B[Original]"));
        _transcript.ShowConversation(firstChat, [first]);
        await WaitAsync(DiagramBlockExpression(first) + "?.dataset.diagramState === 'ready'", "navigation source A renders");
        var received = new TaskCompletionSource<DiagramExportDocument>(TaskCreationOptions.RunContinuationsAsynchronously);
        var release = new TaskCompletionSource<DiagramExportOutcome>(TaskCreationOptions.RunContinuationsAsynchronously);
        _transcript.DiagramExportSaveOverride = document => { received.TrySetResult(document); return release.Task; };
        await EvalAsync<bool>("(() => { document.querySelector('[data-diagram-action=export-svg]').click(); return true; })()");
        var snapshot = await received.Task.WaitAsync(TimeSpan.FromSeconds(10));
        var secondChat = Guid.NewGuid();
        var second = Message(secondChat, "assistant", "CONVERSATION_B_UNCHANGED");
        _transcript.ShowConversation(secondChat, [second]);
        await WaitAsync("transcriptState().conversationId === '" + secondChat + "' && !document.querySelector('.mermaid-block')", "navigation B replaces the old active graph");
        release.SetResult(DiagramExportOutcome.Success);
        await Task.Delay(180);
        Check(Encoding.UTF8.GetString(snapshot.Bytes).Contains("SNAPSHOT_A", StringComparison.Ordinal) && !Encoding.UTF8.GetString(snapshot.Bytes).Contains("CONVERSATION_B", StringComparison.Ordinal), "picker snapshot retains A content after switching to B");
        Check(await EvalAsync<bool>("!document.querySelector('.mermaid-export-status') && __transcriptSmoke.bodyText().includes('CONVERSATION_B_UNCHANGED')"), "late A success creates no feedback or diagram in B");
        _transcript.ShowConversation(firstChat, [first]);
        await WaitAsync(DiagramBlockExpression(first) + "?.dataset.diagramState === 'ready'", "cached A restores its graph");
        Check(await EvalAsync<bool>("!document.querySelector('[data-diagram-action=export-svg]').disabled && !document.querySelector('.mermaid-export-status').textContent"), "restored A does not inherit a pending export or stale success");
        await ExportFileAsync(first, "svg", "diagram-export-after-navigation.svg");
    }

    private async Task CheckExportSafetyAsync()
    {
        var chat = Guid.NewGuid();
        var row = Message(chat, "assistant", MermaidFence("flowchart LR\n A[Safe] --> B[Done]"));
        _transcript.ShowConversation(chat, [row]);
        await WaitAsync(DiagramBlockExpression(row) + "?.dataset.diagramState === 'ready'", "native validation fixture renders");
        int saverCalls = 0;
        _transcript.DiagramExportSaveOverride = _ => { saverCalls++; return Task.FromResult(DiagramExportOutcome.Success); };
        const string validSvg = "<svg xmlns='http://www.w3.org/2000/svg' width='100' height='100' viewBox='0 0 100 100'><rect width='100' height='100' fill='#ffffff'/><text x='8' y='20'>正常 Safe</text></svg>";
        await PostExportProbeAsync(chat, row, "svg", Convert.ToBase64String(Encoding.UTF8.GetBytes(validSvg)), "success");
        Check(saverCalls == 1, "a valid payload with the exact same bridge schema reaches native validation and saver");
        string[] unsafeSvg =
        [
            "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'><script>alert(1)</script><rect width='100' height='100'/></svg>",
            "<!DOCTYPE svg [<!ENTITY x SYSTEM 'file:///invalid'>]><svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'><text>&x;</text></svg>",
            "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'><rect onload='alert(1)'/></svg>",
            "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'><foreignObject><div>unsafe</div></foreignObject></svg>",
            "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'><image href='https://example.invalid/external.svg'/></svg>",
            "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'><rect style='fill:url(https://example.invalid/x)'/></svg>",
            "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100' style='width:99%'><rect width='100' height='100'/></svg>",
            "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100' style='height:100%'><rect width='100' height='100'/></svg>",
            "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'><rect width='100' height='100' style='width:100%;height:100vh'/></svg>",
            "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100' style='background-color:url(https://example.invalid/x)'><rect width='100' height='100'/></svg>",
            "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'><rect width='100' height='100' style='background-color:#ffffff'/></svg>"
        ];
        foreach (string invalid in unsafeSvg)
        {
            await PostExportProbeAsync(chat, row, "svg", Convert.ToBase64String(Encoding.UTF8.GetBytes(invalid.Replace("<svg xmlns", "<svg width='100' height='100' xmlns", StringComparison.Ordinal))));
        }
        await PostExportProbeAsync(chat, row, "png", Convert.ToBase64String(Encoding.UTF8.GetBytes("not a PNG")));
        await PostExportProbeAsync(chat, row, "svg", "not valid base64!!!");
        await PostExportProbeAsync(chat, row, "html", Convert.ToBase64String(Encoding.UTF8.GetBytes(validSvg)));
        Check(saverCalls == 1, "malicious SVG, malformed base64, unsupported formats and fake PNG are rejected before any picker or write");
        var incomplete = Message(chat, "assistant", "```mermaid\nflowchart LR\n A[Partial", "streaming");
        _transcript.ShowConversation(chat, [incomplete]);
        await WaitAsync(DiagramBlockExpression(incomplete) + "?.dataset.diagramState === 'waiting'", "incomplete diagram waits");
        Check(await EvalAsync<bool>("[...document.querySelectorAll('[data-diagram-action^=export-]')].every(button => button.disabled)"), "incomplete diagrams cannot export a misleading image");
        var error = Message(chat, "assistant", MermaidFence("flowchart LR\n A[broken --> B"));
        _transcript.ShowConversation(chat, [error]);
        await WaitAsync(DiagramBlockExpression(error) + "?.dataset.diagramState === 'error'", "bad diagram falls back to source");
        Check(await EvalAsync<bool>("[...document.querySelectorAll('[data-diagram-action^=export-]')].every(button => button.disabled) && document.querySelector('.mermaid-source code').textContent.includes('broken')"), "failed diagrams preserve source and disable SVG/PNG export");
    }

    private async Task CheckExportPreparationAsync()
    {
        var chat = Guid.NewGuid();
        var row = Message(chat, "assistant", MermaidFence("flowchart LR\n A[准备期间 Theme snapshot] --> B[完整图形 Complete]"));
        AppearanceService.ApplyPreferences(AppearanceService.DefaultPaletteId, "light", 15, null);
        _transcript.ShowConversation(chat, [row]);
        await WaitAsync(DiagramBlockExpression(row) + "?.dataset.diagramState === 'ready'", "PNG preparation fixture renders in light mode");
        var received = new TaskCompletionSource<DiagramExportDocument>(TaskCreationOptions.RunContinuationsAsynchronously);
        int saverCalls = 0;
        _transcript.DiagramExportSaveOverride = document => { saverCalls++; received.TrySetResult(document); return Task.FromResult(DiagramExportOutcome.Success); };
        try
        {
            // Hold only the browser's encoded-blob callback to place navigation/theme changes deterministically.
            // 仅延迟浏览器编码完成回调，确定性地验证准备期间的导航与主题变化。
            await EvalAsync<bool>("""
                (() => {
                  window.__originalExportToBlob=HTMLCanvasElement.prototype.toBlob;
                  HTMLCanvasElement.prototype.toBlob=function(callback,...args){__originalExportToBlob.call(this,blob=>{window.__releaseExportBlob=()=>callback(blob);},...args);};
                  window.__prepareSvg=document.querySelector('.mermaid-viewport svg');
                  document.querySelector('[data-diagram-action=export-png]').click();return true;
                })()
                """);
            await WaitAsync("typeof window.__releaseExportBlob==='function'", "PNG reaches the actual encoded-blob callback before native saving");
            Check(await EvalAsync<string>("document.querySelector('.mermaid-export-status').textContent") == UiText.Get("正在准备图表…"), "PNG preparation uses the active English feedback");
            Check(await EvalAsync<bool>("(() => {const buttons=[...document.querySelectorAll('[data-diagram-action^=export-]')];return buttons.length===2 && buttons.every(button=>button.disabled) && document.querySelector('.mermaid-export-controls').getAttribute('aria-busy')==='true';})()"),
                "both export actions expose their busy state and remain disabled during PNG preparation");
            AppearanceService.ApplyPreferences(AppearanceService.DefaultPaletteId, "dark", 15, null);
            await WaitAsync("document.documentElement.dataset.theme==='dark'", "application CSS changes to dark while PNG preparation is pending");
            Check(await EvalAsync<bool>("document.querySelector('.mermaid-viewport svg')===__prepareSvg && getComputedStyle(document.querySelector('.mermaid-viewport')).backgroundColor==='rgb(255, 255, 255)'"),
                "theme changes preserve the pending diagram node and its fixed white content surface");
            await EvalAsync<bool>("(() => { __releaseExportBlob();delete window.__releaseExportBlob;return true; })()");
            var document = await received.Task.WaitAsync(TimeSpan.FromSeconds(10));
            await WaitAsync("document.querySelector('.mermaid-export-status')?.dataset.exportState==='success'", "theme changes preserve the export snapshot and receive success");
            var folder = await StorageFolder.GetFolderFromPathAsync(Path.GetTempPath());
            var file = await folder.CreateFileAsync("diagram-export-theme-snapshot.png", CreationCollisionOption.ReplaceExisting);
            await DiagramExport.WriteAsync(file, document);
            await CheckOpaqueWhitePngAsync("diagram-export-theme-snapshot.png", "PNG retains an opaque white snapshot while the application changes to dark during preparation");
            await EvalAsync<bool>("(() => { document.querySelector('[data-diagram-action=export-png]').click();return true; })()");
            await WaitAsync("typeof window.__releaseExportBlob==='function'", "second PNG preparation can be paused before navigation");
            int previousCalls = saverCalls;
            var nextChat = Guid.NewGuid();
            _transcript.ShowConversation(nextChat, [Message(nextChat, "assistant", "PNG_NAVIGATION_B")]);
            await WaitAsync("transcriptState().conversationId==='" + nextChat + "'", "navigation invalidates the PNG preparation task");
            await EvalAsync<bool>("(() => { __releaseExportBlob();delete window.__releaseExportBlob;return true; })()");
            await Task.Delay(180);
            Check(saverCalls == previousCalls && await EvalAsync<bool>("!document.querySelector('.mermaid-export-status') && __transcriptSmoke.bodyText().includes('PNG_NAVIGATION_B')"),
                "late PNG preparation cannot open a picker or add feedback after navigation");
        }
        finally
        {
            await EvalAsync<bool>("(() => { if(window.__originalExportToBlob) HTMLCanvasElement.prototype.toBlob=__originalExportToBlob;delete window.__releaseExportBlob;return true; })()");
        }
    }

    private async Task PostExportProbeAsync(Guid chat, ConversationMessageViewModel row, string format, string content, string expectedStatus = "error")
    {
        string requestId = "probe-" + Guid.NewGuid().ToString("N");
        var payload = new { type = "exportDiagram", conversationId = chat.ToString(), messageId = row.Message.Id.ToString(), requestId, format, content };
        await EvalAsync<bool>($$"""
            (() => {
              window.__exportProbeResult=null;
              const listener=e=>{if(e.data?.type==='diagramExportResult' && e.data.requestId==={{JsonSerializer.Serialize(requestId)}}){window.__exportProbeResult=e.data;chrome.webview.removeEventListener('message',listener);} };
              chrome.webview.addEventListener('message',listener);
              chrome.webview.postMessage({{JsonSerializer.Serialize(payload)}});
              return true;
            })()
            """);
        await WaitAsync("window.__exportProbeResult?.status === '" + expectedStatus + "'", "native validates export payload " + requestId);
    }

    private async Task CheckExportLayoutAsync()
    {
        UiText.Initialize("zh-CN");
        AppearanceService.ApplyPreferences(AppearanceService.DefaultPaletteId, "light", 15, null);
        var chat = Guid.NewGuid();
        var row = Message(chat, "assistant", "READING_BEFORE\n\n" + MermaidFence(ExportFixtures[0].Source) + "\n\nREADING_AFTER");
        _transcript.ShowConversation(chat, [row]);
        await WaitAsync(DiagramBlockExpression(row) + "?.dataset.diagramState === 'ready'", "layout graph is ready");
        await EvalAsync<bool>("(() => { window.__exportSvg=document.querySelector('.mermaid-viewport svg'); const node=document.querySelector('.message-body > p').firstChild; const r=document.createRange();r.setStart(node,0);r.setEnd(node,7);getSelection().removeAllRanges();getSelection().addRange(r);window.__exportSelection=getSelection().toString(); return true; })()");
        var lightSvg = await ExportFileAsync(row, "svg", "diagram-export-selected.svg");
        var lightPng = await ExportFileAsync(row, "png", "diagram-export-light.png");
        await CheckOpaqueWhitePngAsync("diagram-export-light.png", "light application mode exports an opaque white PNG");
        Check(await EvalAsync<bool>("getSelection().toString()===__exportSelection && document.querySelector('.mermaid-viewport svg')===__exportSvg"), "export preserves text selection and existing diagram nodes");
        await EvalAsync<bool>("(() => { getSelection().removeAllRanges(); return true; })()");
        AppearanceService.ApplyPreferences(AppearanceService.DefaultPaletteId, "dark", 15, null);
        await WaitAsync("document.documentElement.dataset.theme==='dark'", "dark application CSS reaches the existing diagram before export");
        Check(await EvalAsync<bool>("document.querySelector('.mermaid-viewport svg')===__exportSvg && getComputedStyle(document.querySelector('.mermaid-viewport')).backgroundColor==='rgb(255, 255, 255)'"),
            "dark application mode preserves the original diagram node and white content surface");
        var darkSvg = await ExportFileAsync(row, "svg", "diagram-export-dark.svg");
        var darkPng = await ExportFileAsync(row, "png", "diagram-export-dark.png");
        var darkXml = XDocument.Parse(Encoding.UTF8.GetString(darkSvg.Bytes));
        var background = darkXml.Root!.Elements().First(element => element.Name.LocalName == "rect");
        Check(background.Attribute("fill")?.Value == "#ffffff", "standalone SVG keeps a white full-diagram background in dark application mode");
        Check(darkXml.Root.Attribute("style")!.Value.Contains("background-color: #ffffff;", StringComparison.Ordinal),
            "standalone SVG keeps white viewport letterboxing in dark application mode");
        await CheckOpaqueWhitePngAsync("diagram-export-dark.png", "dark application mode exports white corner pixels with opaque alpha across the PNG");
        Check(EquivalentDiagramExports(lightSvg, darkSvg) && lightPng.Bytes.SequenceEqual(darkPng.Bytes),
            "light and dark application modes export the same complete normalized SVG and PNG image");
        Check(!Encoding.UTF8.GetString(darkSvg.Bytes).Contains("var(", StringComparison.Ordinal), "export has no dependency on application CSS variables");
        AppearanceService.ApplyPreferences("sand", "light", 15, "#E91E63");
        string customAccent = AppearanceService.Current.Accent.ToLowerInvariant();
        await WaitAsync("document.documentElement.dataset.theme==='light' && document.documentElement.style.getPropertyValue('--appearance-accent').toLowerCase()===" + JsonSerializer.Serialize(customAccent),
            "custom application palette and accent reach the existing diagram before export");
        var customSvg = await ExportFileAsync(row, "svg", "diagram-export-custom.svg");
        var customPng = await ExportFileAsync(row, "png", "diagram-export-custom.png");
        Check(await EvalAsync<bool>("document.querySelector('.mermaid-viewport svg')===__exportSvg && getComputedStyle(document.querySelector('.mermaid-viewport')).backgroundColor==='rgb(255, 255, 255)'"),
            "custom application palette and accent preserve the original diagram node and white content surface");
        Check(EquivalentDiagramExports(lightSvg, customSvg) && lightPng.Bytes.SequenceEqual(customPng.Bytes),
            "custom application palette and accent export the same complete normalized SVG and PNG image as light mode");
        await CheckOpaqueWhitePngAsync("diagram-export-custom.png", "custom application palette and accent retain an opaque white PNG");
        UiText.Initialize("en");
        _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(480, 820));
        await WaitAsync("document.documentElement.lang==='en' && innerWidth<=480", "export toolbar reaches the English narrow viewport");
        await WaitAsync("document.querySelector('[data-diagram-action=export-svg]').getAttribute('aria-label')==='Export SVG' && document.querySelector('[data-diagram-action=export-png]').getAttribute('aria-label')==='Export PNG' && document.querySelector('.mermaid-export-status').textContent==='Diagram exported'",
            "existing export buttons and visible success feedback switch to English together");
        Check(await EvalAsync<bool>("[...document.querySelectorAll('[data-diagram-action^=export-]')].every(b=>/SVG|PNG/.test(b.getAttribute('aria-label')) && b.getBoundingClientRect().right<=innerWidth && b.getBoundingClientRect().width>=28) && document.documentElement.scrollWidth<=innerWidth+2"), "narrow SVG/PNG buttons remain named, reachable and inside the page");
        Check(await EvalAsync<bool>("!document.querySelector('.mermaid-export-option,[data-diagram-export-scale],[data-diagram-export-transparent]')"),
            "English narrow export toolbar retains the simplified actions without resolution or transparency controls");
        using (var preview = File.Create(Path.Combine(Path.GetTempPath(), "diagram-export-toolbar-480.png")))
            await _transcript.Browser.CoreWebView2.CapturePreviewAsync(Microsoft.Web.WebView2.Core.CoreWebView2CapturePreviewImageFormat.Png, preview.AsRandomAccessStream());
        _metrics["darkExportPngBytes"] = darkPng.Bytes.Length;
    }

    private async Task CheckExportBoundsAsync()
    {
        var chat = Guid.NewGuid();
        var row = Message(chat, "assistant", MermaidFence("flowchart LR\n A[Bounded 中文] --> B[Full geometry]"));
        _transcript.ShowConversation(chat, [row]);
        await WaitAsync(DiagramBlockExpression(row) + "?.dataset.diagramState==='ready'", "bounded output fixture renders");
        // Synthetic dimensions exercise allocation limits without forcing Mermaid to lay out thousands of nodes.
        // 仅合成图形尺寸验证分配上限，不要求 Mermaid 布局数千节点。
        await EvalAsync<bool>("(() => { document.querySelector('.mermaid-viewport svg').setAttribute('viewBox','0 0 100000 100000');return true; })()");
        var svg = await ExportFileAsync(row, "svg", "diagram-export-bounded.svg");
        Check(svg.Width == 100000 && svg.Height == 100000, "vector export retains the complete large viewBox and natural dimensions");
        var png = await ExportFileAsync(row, "png", "diagram-export-bounded.png");
        Check(png.Width == 4000 && png.Height == 4000 && (long)png.Width * png.Height == 16000000,
            "PNG limits pixels before image decode and canvas allocation while retaining the full square aspect ratio");
        Check(await EvalAsync<bool>("document.querySelector('.mermaid-export-status').textContent.length>4 && !document.querySelector('[data-diagram-action=export-png]').disabled"), "reduced PNG succeeds with local feedback and reusable controls");
        Check(await EvalAsync<string>("document.querySelector('.mermaid-export-status').textContent") == UiText.Get("图表已导出") + " · " + UiText.Get("已按尺寸限制缩放导出"),
            "reduced PNG reports the exact active English size explanation");
        byte[] corrupt = png.Bytes.ToArray(); corrupt[^1] ^= 1;
        await ExpectExportRejectedAsync("png", Convert.ToBase64String(corrupt), "native rejects a real PNG with a damaged CRC");
        await ExpectExportRejectedAsync("png", Convert.ToBase64String(png.Bytes[..^12]), "native rejects a PNG whose final chunk is absent");
        await ExpectExportRejectedAsync("svg", new string('A', ((DiagramExport.MaximumSvgBytes + 2) / 3) * 4 + 4), "SVG encoded byte limit is enforced before decoding or XML allocation");
        string deep = "<svg xmlns='http://www.w3.org/2000/svg' width='100' height='100' viewBox='0 0 100 100'>" + string.Concat(Enumerable.Repeat("<g>", 140))
            + "<rect width='100' height='100'/>" + string.Concat(Enumerable.Repeat("</g>", 140)) + "</svg>";
        await ExpectExportRejectedAsync("svg", Convert.ToBase64String(Encoding.UTF8.GetBytes(deep)), "deep XML is bounded before constructing the export object tree");
        var folder = await StorageFolder.GetFolderFromPathAsync(Path.GetTempPath());
        var wrongExtension = await folder.CreateFileAsync("diagram-export-wrong-extension.txt", CreationCollisionOption.ReplaceExisting);
        try { await DiagramExport.WriteAsync(wrongExtension, svg); throw new InvalidOperationException("Mismatched export extension was accepted."); }
        catch (InvalidDataException) { _checks++; }
        Check((await File.ReadAllBytesAsync(wrongExtension.Path)).Length == 0, "mismatched extensions cannot receive diagram bytes");
    }

    private async Task ExpectExportRejectedAsync(string format, string base64, string description)
    {
        try { await DiagramExport.ValidateAsync(format, base64); }
        catch (Exception error) when (error is InvalidDataException or FormatException or System.Xml.XmlException)
        {
            _checks++;
            return;
        }
        throw new InvalidOperationException(description);
    }
}
