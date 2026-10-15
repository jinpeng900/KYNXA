using System.Diagnostics;
using System.Net;
using System.Net.Sockets;
using System.Net.WebSockets;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
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
                await CheckNativeChromeAsync(panel, fixture, conversation);
                panel.ShowResource(conversation, "report.md", "报告.md", userSelected: true, version: "updated");
                await WaitWorkspaceAsync(panel, false, "!!document.querySelector('h1') && !!document.querySelector('.mermaid-svg')");
                Check(panel.DocumentBrowser.CoreWebView2.Settings.IsWebMessageEnabled && !panel.DocumentBrowser.CoreWebView2.Settings.AreHostObjectsAllowed,
                    "trusted local preview returns after PDF and external browsing without exposing host objects");
                _metrics["checks"] = _checks; _metrics["mode"] = "workspace-preview-only";
                _metrics["durationMs"] = cold.ElapsedMilliseconds;
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics));
                File.WriteAllText(_result, $"PASS: {_checks} native workspace preview checks; Markdown/table/math/Mermaid, scroll restoration, text, image, SVG, PDF, isolated HTML, cached DOM and actual embedded Chrome HWND/CDP body.");
            }
            catch (Exception error) { File.WriteAllText(_result, "FAIL: " + error); }
            finally { panel.Dispose(); await fixture.DisposeAsync(); _window?.Close(); }
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
            throw new InvalidOperationException("Workspace preview timed out: " + expression + "; source=" + lastSource);
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
        public TaskCompletionSource SlowStarted { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public TaskCompletionSource ReleaseSlow { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);

        public WorkspacePreviewFixture()
        {
            _listener.Start(); Address = new Uri($"http://127.0.0.1:{((IPEndPoint)_listener.LocalEndpoint).Port}");
            _server = ServeAsync();
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
                    if (target == "/agent/workspace/preview")
                    {
                        PreviewRequests++;
                        using var json = JsonDocument.Parse(requestBody);
                        string path = json.RootElement.GetProperty("path").GetString()!;
                        if (path == "slow.md")
                        {
                            SlowRequests++; SlowStarted.TrySetResult();
                            await ReleaseSlow.Task.WaitAsync(_stop.Token);
                        }
                        LastUserSelected = json.RootElement.GetProperty("userSelected").GetBoolean();
                        string kind = Path.GetExtension(path) switch { ".md" => "markdown", ".mmd" => "mermaid", ".png" or ".svg" => "image", ".pdf" => "pdf", ".html" => "html", _ => "text" };
                        string text = kind == "markdown" ? "# REPORT_FIXTURE\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n$E=mc^2$\n\n```mermaid\nflowchart LR\n A-->B\n```\n\n" + string.Join("\n\n", Enumerable.Range(0, 300).Select(index => $"段落 {index}：用于核对实际文件切换后的阅读位置。"))
                            : kind == "mermaid" ? "flowchart LR\n A[FILE_DIAGRAM]-->B[完成]" : path == "sample.cs" ? "public class Sample { public string Name = \"syntax\"; }" : path == "sample.rs" ? "// 中文说明\npub fn greet() -> String { String::from(\"Hello\") }" : "PLAIN_FILE_FIXTURE\n第二行";
                        if (path == "slow.md") text = "# BACKGROUND_DOCUMENT\n\n```mermaid\nflowchart LR\n A[后台加载]-->B[完成并缓存]\n```";
                        bytes = JsonSerializer.SerializeToUtf8Bytes(new { kind, title = path, path, version = "fixture-v1", text,
                            mimeType = kind == "pdf" ? "application/pdf" : "text/plain", hasMore = false,
                            contentUrl = kind is "pdf" or "image" or "html" ? "/agent/workspace/content?file=" + path : null as string });
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
