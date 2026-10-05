using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;

namespace TranscriptUiSmoke;

public partial class App
{
    private async Task CheckWebsiteActivitiesAsync()
    {
        var chat = Guid.NewGuid();
        var reply = Message(chat, "assistant", "网站活动验证：以下网址均为隔离样例，不打开网站。", "streaming");
        string longUrl = "https://fetch.example.invalid/" + new string('a', 220) + "?part=one&part=two";
        string McpText(string text) => JsonSerializer.Serialize(new
        {
            content = new[] { new { type = "text", text } },
            _meta = new { url = "https://private-meta.example.invalid/hidden" }
        });
        JsonElement WebsiteArgs(object arguments) => JsonSerializer.SerializeToElement(new
        {
            arguments,
            policy = new { reason = "Fixture approval https://private-policy.example.invalid/hidden" },
            _meta = new { url = "https://private-args-meta.example.invalid/hidden" }
        });
        reply.Message.ToolActivities =
        [
            new("web-fetch", "mcp.fetch.fetch", WebsiteArgs(new { url = longUrl }), "running", "Fetch fixture website"),
            new("web-exa", "mcp.exa.web_search_exa", WebsiteArgs(new { query = "fixture verified report" }), "completed", "Search public fixtures",
                McpText("Title: First source\nURL: https://exa-one.example.invalid/report\nContent: Fixture one\n\nTitle: Second source\nURL: https://exa-two.example.invalid/report\nContent: Fixture two\n\nURL: https://exa-one.example.invalid/report")),
            new("web-playwright", "mcp.playwright.browser_navigate", WebsiteArgs(new { url = "https://initial.example.invalid/requested" }), "completed", "Navigate fixture website",
                "### Ran Playwright code\nawait page.goto('https://initial.example.invalid/requested');\n### Page\n- Page URL: https://final.example.invalid/after-redirect\n- Page Title: Fixture\n### Snapshot\n- link: https://snapshot-link.example.invalid/unrelated"),
            new("web-chrome", "mcp.chrome-devtools.list_pages", WebsiteArgs(new { }), "completed", "Inspect current fixture page",
                McpText("# list_pages response\n0: about:blank\n1: https://selected.example.invalid/current [selected]\n2: https://background.example.invalid/other-tab")),
            new("web-query", "mcp.brave.brave_web_search", WebsiteArgs(new { query = "fixture query without returned URLs" }), "running", "Search fixture"),
            new("web-javascript", "mcp.fetch.fetch", WebsiteArgs(new { url = "javascript:alert('fixture')" }), "error", "Unsafe fixture input", "No website was fetched."),
            new("web-file", "mcp.fetch.fetch", WebsiteArgs(new { url = "file:///fixture/private.txt" }), "error", "Unsafe fixture input", "No website was fetched."),
            new("file-url", "filesystem.read", JsonSerializer.SerializeToElement(new { path = "fixture.txt" }), "completed", "Read local fixture", "Local file mentions https://file-output.example.invalid/source"),
            new("custom-url", "mcp.fixture.custom", WebsiteArgs(new { url = "https://custom.example.invalid/business" }), "completed", "Generic fixture response", McpText("https://generic-result.example.invalid/source"))
        ];
        var generic = reply.Message.ToolActivities[^1];
        reply.Message.ToolActivities.RemoveAt(reply.Message.ToolActivities.Count - 1);
        reply.Message.ToolActivities.Insert(0, generic);
        // Keep this additional capability in its own reply: the active projection deliberately
        // retains only eight ordinary activities per message, preserving the full event list.
        // 将额外能力放到单独回复；活动投影刻意每条消息仅显示八项普通活动，但保留完整事件列表。
        var builtinReply = Message(chat, "assistant", "内置网页读取验证：不打开真实网站。", "streaming");
        builtinReply.Message.ToolActivities = [new("web-builtin", "web.fetch",
            JsonSerializer.SerializeToElement(new { url = "https://builtin.example.invalid/source", reason = "Read synthetic public source." }),
            "completed", "Read public fixture", JsonSerializer.Serialize(new { url = "https://builtin.example.invalid/source", content = "Builtin evidence fixture" }))];
        _transcript.ShowConversation(chat, [reply, builtinReply]);
        await WaitAsync("document.querySelectorAll('.tool-web-link').length === 6 && document.querySelectorAll('.tool-activity').length <= 9", "real DTO distinguishes website rows from generic tool details");
        Check(await EvalAsync<bool>("document.querySelector('[data-tool-call-id=web-builtin] .tool-title').textContent === '阅读网页' && document.querySelector('[data-tool-call-id=web-builtin] .tool-web-link').href === 'https://builtin.example.invalid/source' && !document.querySelector('[data-tool-call-id=web-builtin] details')"), "builtin web.fetch displays a direct website link without JSON panels");
        Check(await EvalAsync<bool>("!document.querySelector('.tool-activities summary') && [...document.querySelectorAll('.tool-web-activity')].every(row => row.tagName === 'DIV')"), "website sources are directly visible without disclosure cards");
        await EvalAsync<bool>("(() => { scrollTo(0,0); return true; })()");
        await Task.Delay(80);
        Check(await EvalAsync<bool>("[...document.querySelectorAll('.tool-web-activity')].every(row => !row.querySelector('details,summary,.tool-details,.tool-arguments,.tool-result,.tool-metadata,button'))"), "website rows expose links directly without argument/result JSON details");
        Check(await EvalAsync<bool>("document.querySelector('[data-tool-call-id=web-exa] .tool-title').textContent === '搜索网页' && document.querySelector('[data-tool-call-id=web-fetch] .tool-title').textContent === '阅读网页' && [...document.querySelectorAll('.tool-title')].every(node=>!node.textContent.startsWith('mcp.'))"), "website activities use friendly search and reading labels without MCP names");
        Check(await EvalAsync<bool>("document.querySelector('[data-tool-call-id=web-fetch] .tool-web-link').href === " + JsonSerializer.Serialize(longUrl) + " && document.querySelector('[data-tool-call-id=web-fetch] .tool-web-link').checkVisibility()"), "Fetch displays its complete requested HTTP URL immediately");
        Check(await EvalAsync<bool>("[...document.querySelectorAll('[data-tool-call-id=web-exa] .tool-web-link')].map(link=>link.href).join('|') === 'https://exa-one.example.invalid/report|https://exa-two.example.invalid/report'"), "Exa renders multiple source URLs once in order");
        Check(await EvalAsync<bool>("[...document.querySelectorAll('[data-tool-call-id=web-playwright] .tool-web-link')].map(link=>link.href).join('|') === 'https://final.example.invalid/after-redirect|https://selected.example.invalid/current'"), "adjacent completed browser actions merge while retaining both exact current page URLs");
        Check(await EvalAsync<bool>("[...document.querySelectorAll('.tool-web-link')].some(link=>link.href === 'https://selected.example.invalid/current') && !document.querySelector('[data-tool-call-id=web-playwright]').textContent.includes('background.example.invalid')"), "Chrome shows only the selected current page");
        Check(await EvalAsync<bool>("document.querySelector('[data-tool-call-id=web-query] .tool-command').textContent === 'fixture query without returned URLs' && !document.querySelector('[data-tool-call-id=web-query] .tool-command').hidden"), "running search without a URL retains a brief real query");
        Check(await EvalAsync<bool>("[...document.querySelectorAll('.tool-web-activity')].filter(row=>row.querySelector('.tool-web-link')).every(row=>row.querySelector('.tool-command').hidden)"), "website URLs replace the duplicate command preview");
        Check(await EvalAsync<bool>("[...document.querySelectorAll('.tool-web-link')].every(link=>['http:','https:'].includes(new URL(link.href).protocol) && link.tabIndex >= 0 && !/private-|background|initial|snapshot-link|custom|generic-result|file-output/.test(link.href)) && !document.querySelector('[data-tool-call-id=web-javascript] .tool-web-link,[data-tool-call-id=web-file] .tool-web-link')"), "policy/meta, unsafe schemes and unrelated pages never become website links");
        Check(await EvalAsync<bool>("[...document.querySelectorAll('.tool-web-activity')].every(row=>!/private-|javascript:|file:/.test(row.textContent))"), "website rows omit policy/meta and unsafe URL text rather than displaying it as a command");
        Check(await EvalAsync<bool>("document.querySelector('[data-tool-call-id=file-url]').tagName === 'DIV' && !document.querySelector('[data-tool-call-id=file-url] .tool-web-link') && KynxaToolWebLinks.extract({name:'mcp.fixture.custom',arguments:{url:'https://custom.example.invalid/business'}}) === null"), "file output and unknown MCP URLs remain generic actions without web classification");
        Check(await EvalAsync<bool>("[...document.querySelectorAll('.tool-web-link')].every(link=>getComputedStyle(link).fontSize === '12px' && getComputedStyle(link).color === 'rgb(115, 115, 115)')"), "website links use the small gray command style");
        await EvalAsync<bool>("(() => { window.__websiteRow = document.querySelector('[data-tool-call-id=web-fetch]'); window.__websiteLink = window.__websiteRow.querySelector('.tool-web-link'); const range = document.createRange(); range.selectNodeContents(window.__websiteLink); getSelection().removeAllRanges(); getSelection().addRange(range); window.__websiteAnchor = getSelection().anchorNode; window.__websiteAnchorOffset = getSelection().anchorOffset; return true; })()");
        Check(await EvalAsync<string>("transcriptSelectionText()") == longUrl, "selecting the entire website copies its complete URL");
        string language = UiText.Language;
        try
        {
            UiText.Initialize("en");
            await WaitAsync("document.querySelector('[data-tool-call-id=web-fetch] .tool-state').textContent === 'Running' && document.querySelector('[data-tool-call-id=web-exa] .tool-state').textContent === 'Completed'", "website activity status switches language immediately");
            Check(await EvalAsync<bool>("window.__websiteRow === document.querySelector('[data-tool-call-id=web-fetch]') && window.__websiteLink === window.__websiteRow.querySelector('.tool-web-link') && getSelection().anchorNode === window.__websiteAnchor && getSelection().anchorOffset === window.__websiteAnchorOffset") && await EvalAsync<string>("transcriptSelectionText()") == longUrl,
                "language change preserves website link DOM and exact selected URL");
            UiText.Initialize("zh-CN");
            await WaitAsync("document.querySelector('[data-tool-call-id=web-fetch] .tool-state').textContent === '执行中'", "website status restores Chinese without translating URLs");
        }
        finally { UiText.Initialize(language); }
        reply.Message.ToolActivities[1] = reply.Message.ToolActivities[1] with { Status = "completed", Result = McpText("Contents of " + longUrl + ":\nFixture website content") };
        reply.Refresh();
        await WaitAsync("transcriptState().pending", "website result update waits while its URL is selected");
        Check(await EvalAsync<bool>("document.querySelector('[data-tool-call-id=web-fetch] .tool-state').dataset.status === 'running' && window.__websiteLink === document.querySelector('[data-tool-call-id=web-fetch] .tool-web-link')") && await EvalAsync<string>("transcriptSelectionText()") == longUrl,
            "selected website URL survives a deferred execution result");
        _transcript.ClearSelection();
        await WaitAsync("!transcriptState().selection", "website selection clears before result update");
        await WaitAsync("document.querySelector('[data-tool-call-id=web-fetch] .tool-state').dataset.status === 'completed'", "website execution status updates through real C# DTO");
        Check(await EvalAsync<bool>("window.__websiteRow === document.querySelector('[data-tool-call-id=web-fetch]') && window.__websiteLink === window.__websiteRow.querySelector('.tool-web-link') && document.querySelectorAll('[data-tool-call-id=web-fetch] .tool-web-link').length === 1"), "website completion preserves row/link identity and deduplicates the returned URL");

        _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(480, 850));
        await Task.Delay(120);
        Check(await EvalAsync<bool>("document.documentElement.scrollWidth <= innerWidth && [...document.querySelectorAll('.tool-web-link')].every(link=>link.getBoundingClientRect().right <= innerWidth) && window.__websiteLink.textContent === window.__websiteLink.href"), "compact long URLs respect narrow chat width while retaining complete selectable text");
        _metrics["websiteActivityNarrowPreview"] = Path.Combine(Path.GetTempPath(), "kynxa-transcript-website-links-480.png");
        await CaptureViewportAsync((string)_metrics["websiteActivityNarrowPreview"]);
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 850));
        await Task.Delay(120);
        _metrics["websiteActivityPreview"] = Path.Combine(Path.GetTempPath(), "kynxa-transcript-website-links.png");
        await CaptureViewportAsync((string)_metrics["websiteActivityPreview"]);

        var sources = Enumerable.Range(0, 32).Select(index => "https://source-" + index + ".example.invalid/report").ToArray();
        var manyReply = Message(Guid.NewGuid(), "assistant", "SOURCE_CAP_FIXTURE", "streaming");
        manyReply.Message.ToolActivities = [new("many-sources", "mcp.exa.web_search_exa", WebsiteArgs(new { query = "fixture many sources" }),
            "completed", "Search fixtures", McpText(string.Join("\n", sources.Select(url => "URL: " + url))))];
        _transcript.ShowConversation(manyReply.ConversationId, [manyReply]);
        await WaitAsync("document.querySelectorAll('.tool-web-link').length === 4 && document.querySelector('.tool-web-more').textContent.includes('28')", "many sources show four real links and a static accurate remainder count");
        Check(await EvalAsync<bool>("!document.querySelector('.tool-web-more button,.tool-web-more a')"), "source remainder is plain text rather than another expandable panel");
        Check(manyReply.Message.ToolActivities[0].Result!.Contains(sources[^1])
            && reply.Message.ToolActivities.Count == 9 && builtinReply.Message.ToolActivities.Count == 1,
            "source caps and compact website merging leave full original URL events intact");
    }
}
