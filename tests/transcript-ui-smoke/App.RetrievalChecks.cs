using System.Text.Json;
using KYNXA_Desktop.Services;

namespace TranscriptUiSmoke;

public partial class App
{
    private async Task CheckRetrievalToolsAsync()
    {
        string language = UiText.Language;
        var chat = Guid.NewGuid();
        var reply = Message(chat, "assistant", "RETRIEVAL_PROGRESS 正在核对工作资料与公开来源。", "streaming");
        reply.Message.ToolActivities =
        [
            new("web-unified", "web.search", JsonSerializer.SerializeToElement(new { query = "Synthetic public evidence", reason = "Verify synthetic source" }),
                "completed", "web.search", JsonSerializer.Serialize(new { structuredContent = new { sources = new[]
                { new { url = "https://first.example.invalid/source", title = "First source" }, new { url = "https://second.example.invalid/source", title = "Second source" } },
                    _meta = new { url = "https://private-meta.example.invalid/hidden" } } }), Round: 1, Order: 1),
            new("knowledge-find", "knowledge.search", JsonSerializer.SerializeToElement(new { query = "中文工作约定" }), "completed", "knowledge.search",
                JsonSerializer.Serialize(new { items = new[] { new { text = "PRIVATE_RESULT_JSON_FIXTURE", sourceRef = new { id = "synthetic-source" } } } }), Round: 1, Order: 2),
            new("knowledge-read", "knowledge.read", JsonSerializer.SerializeToElement(new { sourceRef = new { id = "synthetic-source" } }), "completed", "knowledge.read",
                JsonSerializer.Serialize(new { text = "PRIVATE_RESULT_JSON_FIXTURE" }), Round: 1, Order: 3)
        ];
        reply.Message.AssistantSegments = [new("retrieval-progress", 1, 0, "commentary", "completed", reply.Message.Content, string.Empty)];
        try
        {
            UiText.Initialize("zh-CN");
            _transcript.ShowConversation(chat, [reply]);
            await WaitAsync("document.querySelectorAll('.tool-activity').length === 3 && document.querySelectorAll('.tool-web-link').length === 2",
                "actual retrieval DTOs show three compact activities and two website source links");
            Check(await EvalAsync<bool>("[...document.querySelectorAll('.tool-title')].map(node=>node.textContent).join(',') === '搜索网页,查找资料,读取资料'"),
                "unified web search and local retrieval use readable Chinese labels");
            Check(await EvalAsync<bool>("[...document.querySelectorAll('[data-tool-call-id=web-unified] .tool-web-link')].map(link=>link.href).join('|') === 'https://first.example.invalid/source|https://second.example.invalid/source'"),
                "normalized sources render as direct ordered website links");
            Check(await EvalAsync<bool>("!document.querySelector('.tool-arguments,.tool-result,.tool-details,.tool-metadata') && !document.getElementById('messages').textContent.includes('PRIVATE_RESULT_JSON_FIXTURE') && !document.getElementById('messages').textContent.includes('private-meta')"),
                "retrieval rows do not render argument/result JSON or private metadata");
            Check(await EvalAsync<bool>("document.querySelector('[data-tool-call-id=knowledge-find] .tool-command').textContent === '中文工作约定' && !document.querySelector('[data-tool-call-id=knowledge-read] .tool-web-link')"),
                "local retrieval retains its short query without impersonating a website action");
            await EvalAsync<bool>("(() => { window.__retrievalRow = document.querySelector('[data-tool-call-id=web-unified]'); return true; })()");
            UiText.Initialize("en");
            await WaitAsync("[...document.querySelectorAll('.tool-title')].map(node=>node.textContent).join(',') === 'Search web,Find sources,Read source'",
                "retrieval action labels switch to English immediately");
            Check(await EvalAsync<bool>("window.__retrievalRow === document.querySelector('[data-tool-call-id=web-unified]')"),
                "live language changes preserve the existing website row");
            reply.Message.AssistantSegments.Add(new("retrieval-final", 2, 4, "final_answer", "completed", "FINAL_RETRIEVAL_VERIFIED\n\n**Complete.**", string.Empty));
            reply.Message.Content = reply.Message.AssistantSegments[^1].Content;
            reply.Refresh();
            await WaitAsync("document.querySelectorAll('.assistant-segment').length === 2 && document.querySelectorAll('.tool-activity').length === 3 && !document.querySelector('.final-answer')",
                "receiving a final segment preserves intermediate prose and retrieval activities until terminal success");
            reply.Message.Status = "completed";
            reply.Message.DurationMs = 4200;
            reply.Refresh();
            await WaitAsync("document.querySelector('.final-answer .message-body strong') !== null && !document.querySelector('.tool-activity,.reasoning') && document.querySelector('.message-elapsed').textContent.includes('5')",
                "successful final render removes retrieval process DOM and shows the real total elapsed time");
            Check(reply.Message.ToolActivities.Count == 3 && reply.Message.AssistantSegments.Count == 2,
                "final convergence retains all source/tool events in formal message data");
        }
        finally { UiText.Initialize(language); }
    }
}
