using System.Text.Json;
using KYNXA_Desktop.Controls;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Automation.Provider;
using Microsoft.UI.Xaml.Controls;

namespace TranscriptUiSmoke;

public partial class App
{
    private async Task CheckWorkspaceFormatsAsync(ConversationDocumentPanel panel, WorkspacePreviewFixture fixture, Guid conversation)
    {
        void InvokeToolbar(string field)
        {
            var button = (Button)typeof(ConversationDocumentPanel).GetField(field,
                System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic)!.GetValue(panel)!;
            ((IInvokeProvider)new ButtonAutomationPeer(button).GetPattern(PatternInterface.Invoke)).Invoke();
        }
        foreach (string path in new[] { "data.csv", "data.tsv" })
        {
            panel.ShowResource(conversation, path);
            await WaitWorkspaceAsync(panel, false, "document.querySelectorAll('table tr').length===2 && document.querySelector('.copy-table')!==null");
            var view = panel.DocumentBrowser!.CoreWebView2;
            Check(await view.ExecuteScriptAsync("document.querySelectorAll('table tr')[1].cells[1].textContent==='first\\nsecond'") == "true",
                path + " keeps a quoted multiline field inside one cell");
            InvokeToolbar("_more");
            await WaitWorkspaceAsync(panel, false, "document.querySelectorAll('table tr').length===3 && document.querySelectorAll('table tr')[2].cells[0].textContent==='Two'");
            Check(await view.ExecuteScriptAsync("document.querySelectorAll('table tr')[2].cells[1].textContent===" + JsonSerializer.Serialize(path.EndsWith("csv") ? "a,b" : "a\tb")) == "true",
                path + " appends complete records without splitting quoted delimiters");
            InvokeToolbar("_source");
            await WaitWorkspaceAsync(panel, false, "document.querySelector('pre.plain-text')?.textContent.includes('first\\nsecond') && document.querySelector('pre.plain-text')?.textContent.includes('Two')");
            InvokeToolbar("_source");
            await WaitWorkspaceAsync(panel, false, "document.querySelectorAll('table tr').length===3");
        }

        Guid mount = Guid.NewGuid();
        panel.ShowResource(conversation, "reports/formats.md", mountId: mount);
        await WaitWorkspaceAsync(panel, false, "!!document.querySelector('#format-report') && !!document.querySelector('.markdown-alert') && !!document.querySelector('.footnotes') && !!document.querySelector('.copy-block') && document.querySelector('img[alt=Allowed]')?.naturalWidth>0 && document.querySelector('img[alt=Remote]')?.naturalWidth>0 && document.querySelector('img[alt=Denied]')?.complete===true");
        var markdown = panel.DocumentBrowser!.CoreWebView2;
        Check(await markdown.ExecuteScriptAsync("document.querySelector('img[alt=Denied]').naturalWidth===0 && [...document.querySelectorAll('.markdown-image')].every(img=>new URL(img.src).hostname==='kynxa-markdown-images.local')") == "true",
            "authorized images cross the proxy while the denied image never displays");
        Check(fixture.DeniedImageRequests == 1 && fixture.RemoteImageRequests == 1
            && fixture.FormatPreviewRequests.Where(request => request.Path.EndsWith(".png")).All(request => !request.UserSelected && request.MountId == mount)
            && fixture.FormatPreviewRequests.Any(request => request.Path.Replace('\\', '/') == "reports/images/picture.png"),
            "image reads preserve the document directory, mount and non-picker authorization");
        await markdown.ExecuteScriptAsync("window.__formatHeading=document.querySelector('#format-report'); document.querySelector('a[href=\"#target-anchor\"]').click()");
        await WaitWorkspaceAsync(panel, false, "scrollY>300");
        await Task.Delay(160);

        string? openedPath = null;
        panel.FileRequested += (_, request) =>
        {
            openedPath = request.Path;
            int marker = request.Path.IndexOf('#');
            panel.ShowResource(request.ConversationId, marker < 0 ? request.Path : request.Path[..marker], mountId: request.MountId);
            if (marker >= 0) panel.NavigateToAnchor(request.Path[(marker + 1)..]);
        };
        await markdown.ExecuteScriptAsync("document.querySelector('a[href=\"next.md#next-section\"]').click()");
        await WaitWorkspaceAsync(panel, false, "document.querySelector('h1')?.textContent==='NEXT_FILE_FIXTURE'");
        Check(openedPath?.Replace('\\', '/') == "reports/next.md#next-section", "a clicked relative file is resolved by the native document owner");
        panel.ShowResource(conversation, "reports/formats.md", mountId: mount);
        await WaitWorkspaceAsync(panel, false, "window.__formatHeading===document.querySelector('#format-report') && scrollY>300");
        Check(ReferenceEquals(markdown, panel.DocumentBrowser!.CoreWebView2), "returning to a file preserves its existing DOM and reading position");

        string? website = null;
        panel.WebsiteRequested += (_, url) => { website = url; panel.ShowWebsite(conversation, url); };
        await markdown.ExecuteScriptAsync("[...document.querySelectorAll('a')].find(a=>a.textContent==='Website').click()");
        nint owner = Microsoft.UI.Win32Interop.GetWindowFromWindowId(_window!.AppWindow.Id);
        DateTime deadline = DateTime.UtcNow.AddSeconds(20);
        // A discovered HWND precedes native reparenting; wait for the actual embedded child, not discovery alone.
        // 发现 HWND 早于原生挂载；等待真实子窗口关系，而不是把发现窗口当作已嵌入。
        while ((panel.ChromeBrowser?.BrowserWindow is not > 0 || !NativeChromeIsChild(owner, panel.ChromeBrowser.BrowserWindow))
            && DateTime.UtcNow < deadline) await Task.Delay(40);
        string notices = string.Join(" | ", WorkTabVisuals(panel).OfType<TextBlock>().Select(text => text.Text));
        Check(website == new Uri(fixture.Address, "/site").AbsoluteUri && panel.ChromeBrowser?.BrowserWindow is > 0
            && (await fixture.ChromeBodyAsync()).Contains("BROWSER_SITE_FIXTURE"), $"a clicked website opens in the owned workspace browser rather than the system browser; url={website}; window={panel.ChromeBrowser?.BrowserWindow}; notice={notices}");
        Check(NativeChromeIsChild(owner, panel.ChromeBrowser!.BrowserWindow), "the website is actually embedded inside the isolated test window");
        panel.ShowResource(conversation, "reports/formats.md", mountId: mount);
        await WaitWorkspaceAsync(panel, false, "window.__formatHeading===document.querySelector('#format-report') && scrollY>300");
        Check(ReferenceEquals(markdown, panel.DocumentBrowser!.CoreWebView2), "browser navigation does not replace or reset the original file tab");
    }
}
