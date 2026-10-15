using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;

namespace TranscriptUiSmoke;

public partial class App
{
    private async Task CheckSteeringReplyAsync()
    {
        string? reportPath = Environment.GetEnvironmentVariable("KYNXA_TRANSCRIPT_API_REPORT");
        if (!string.IsNullOrWhiteSpace(reportPath))
        {
            await CheckApiSteeringReplyAsync(reportPath);
            return;
        }
        var conversationId = Guid.NewGuid();
        var original = new ChatMessageState { Role = "user", Content = "ORIGINAL_REQUEST 分析文档并写出产物" };
        var replaced = new ChatMessageState
        {
            Role = "assistant", Content = "SUPERSEDED_REPLY 之前的执行说明", InReplyToUserMessageId = original.Id,
            OriginUserMessageId = original.Id, Status = "streaming"
        };
        _transcript.ShowConversation(conversationId, [new(conversationId, original), new(conversationId, replaced)]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('SUPERSEDED_REPLY')", "the old reply was actually rendered before guidance");
        var guidance = new ChatMessageState { Role = "user", Content = "GUIDANCE_REQUEST 改为比较成本并保留已有证据" };
        replaced.SupersededByUserMessageId = guidance.Id;
        replaced.Status = "interrupted";
        const string markdown = "# 最终交付\n\n已按调整继续完成 **成本比较**。\n\n| 项目 | 结果 |\n| --- | --- |\n| 已有证据 | 保留 |\n\n```text\nFINAL_ARTIFACT_READY\n```";
        var final = new ChatMessageState
        {
            Role = "assistant", Content = markdown, Status = "completed", InReplyToUserMessageId = guidance.Id,
            OriginUserMessageId = original.Id
        };
        ChatMessageState[] formal = [original, replaced, guidance, final];
        string stored = JsonSerializer.Serialize(formal);
        var restored = JsonSerializer.Deserialize<ChatMessageState[]>(stored)
            ?? throw new InvalidOperationException("Formal steering history could not be restored.");
        Check(restored.Length == 4 && restored[1].SupersededByUserMessageId == guidance.Id && restored[1].Status == "interrupted"
            && restored[3].InReplyToUserMessageId == guidance.Id && restored[3].OriginUserMessageId == original.Id,
            "formal JSON retains the stopped prior reply and current guidance linkage");
        _transcript.ShowConversation(conversationId, restored.Select(message => new ConversationMessageViewModel(conversationId, message)).ToArray());
        await WaitAsync("window.transcriptState().messageCount === 4 && __transcriptSmoke.bodyText().includes('FINAL_ARTIFACT_READY')",
            "guidance continuation retains prior output above the new guidance and final answer");
        await CheckSteeringReplyDomAsync(original.Id, replaced.Id, guidance.Id, final.Id);
        Check(JsonSerializer.Serialize(restored) == stored, "rendering leaves the full formal history and supersession markers intact");

        // Reopen from another JSON read, rather than relying on the already-rendered row instances.
        // 再次读取正式 JSON 后重新打开，避免仅验证当前已渲染的行实例。
        _transcript.ShowConversation(Guid.NewGuid(), []);
        await WaitAsync("window.transcriptState().messageCount === 0", "intermediate navigation clears the current transcript");
        var reopened = JsonSerializer.Deserialize<ChatMessageState[]>(JsonSerializer.Serialize(restored))!;
        _transcript.ShowConversation(conversationId, reopened.Select(message => new ConversationMessageViewModel(conversationId, message)).ToArray());
        await WaitAsync("window.transcriptState().messageCount === 4 && __transcriptSmoke.bodyText().includes('FINAL_ARTIFACT_READY')",
            "history reload retains prior output and the completed continuation in order");
        await CheckSteeringReplyDomAsync(original.Id, replaced.Id, guidance.Id, final.Id);
        Check(reopened[3].Content == markdown && reopened[3].Status == "completed", "completed final Markdown survives persistence and reload exactly");
    }

    // Replay the explicitly supplied API history through production rendering; never dispatch another model request.
    // 将明确指定的真实 API 历史交给正式渲染路径；不再次派发模型请求。
    private async Task CheckApiSteeringReplyAsync(string reportPath)
    {
        using var report = JsonDocument.Parse(await File.ReadAllBytesAsync(reportPath));
        Check(report.RootElement.GetProperty("passed").GetBoolean(), "the source API steering run completed successfully");
        var scenarios = report.RootElement.GetProperty("scenarios");
        Check(scenarios.GetArrayLength() == 1, "the report contains the single requested API steering scenario");
        var scenario = scenarios[0];
        var input = scenario.GetProperty("input");
        var guidance = scenario.GetProperty("guidance");
        Guid conversationId = input.GetProperty("conversationId").GetGuid();
        Guid originalId = input.GetProperty("userMessageId").GetGuid();
        Guid previousReplyId = input.GetProperty("requestId").GetGuid();
        Guid guidanceId = guidance.GetProperty("userMessageId").GetGuid();
        Guid currentReplyId = guidance.GetProperty("replyMessageId").GetGuid();
        var terminal = scenario.GetProperty("terminal").Deserialize<ChatStreamEvent>(new JsonSerializerOptions(JsonSerializerDefaults.Web))!;
        Check(terminal.Type == "completed", "the real API delivered a completed continuation reply");
        var messages = scenario.GetProperty("formalMessages").Deserialize<ChatMessageState[]>()!;
        var previous = messages.Single(message => message.Id == previousReplyId);
        var current = messages.Single(message => message.Id == currentReplyId);
        Check(previous.SupersededByUserMessageId == guidanceId && previous.Status != "streaming"
            && current.InReplyToUserMessageId == guidanceId && current.OriginUserMessageId == originalId
            && current.Status == "completed", "formal API records freeze prior output and bind the new reply to guidance on the original task");
        string stored = JsonSerializer.Serialize(messages);
        var expectedOrder = new[] { originalId.ToString(), previousReplyId.ToString(), guidanceId.ToString(), currentReplyId.ToString() };
        var previousProjection = TranscriptPresentation.Select(previous.Role, previous.Status, previous.Content,
            previous.AssistantSegments, previous.ToolActivities, previous.TaskCompletionState);
        var currentProjection = TranscriptPresentation.Select(current.Role, current.Status, current.Content,
            current.AssistantSegments, current.ToolActivities, current.TaskCompletionState);
        Check(!string.IsNullOrWhiteSpace(previousProjection.Content) && currentProjection.Mode == "final"
            && !string.IsNullOrWhiteSpace(currentProjection.Content), "both real prior output and the full final answer are available for rendering");
        string[] previousHtml = previousProjection.Segments.Length == 0
            ? [TranscriptMarkdown.Render(previousProjection.Content)]
            : previousProjection.Segments.Where(segment => !string.IsNullOrWhiteSpace(segment.Content))
                .Select(segment => TranscriptMarkdown.Render(segment.Content)).ToArray();
        string finalHtml = TranscriptMarkdown.Render(currentProjection.Content);
        _metrics["apiReportPath"] = Path.GetFullPath(reportPath);
        _metrics["sourceApiPassed"] = true;
        _metrics["apiSteeringCharacters"] = new { previous = previousProjection.Content.Length, final = currentProjection.Content.Length };

        _transcript.ShowConversation(conversationId, messages.Select(message => new ConversationMessageViewModel(conversationId, message)).ToArray());
        await CheckApiSteeringDomAsync(expectedOrder, previousReplyId, currentReplyId, previousHtml, finalHtml);
        Check(JsonSerializer.Serialize(messages) == stored, "native rendering leaves all real formal messages unchanged");
        _transcript.ShowConversation(Guid.NewGuid(), []);
        await WaitAsync("window.transcriptState().messageCount === 0", "navigation leaves the previous API conversation");
        var reopened = JsonSerializer.Deserialize<ChatMessageState[]>(stored)!;
        _transcript.ShowConversation(conversationId, reopened.Select(message => new ConversationMessageViewModel(conversationId, message)).ToArray());
        await CheckApiSteeringDomAsync(expectedOrder, previousReplyId, currentReplyId, previousHtml, finalHtml);
        Check(JsonSerializer.Serialize(reopened) == stored, "reopened history preserves the full real prior output and final reply");
    }

    private async Task CheckApiSteeringDomAsync(string[] expectedOrder, Guid previousReplyId, Guid currentReplyId,
        string[] previousHtml, string finalHtml)
    {
        await WaitAsync("window.transcriptState().messageCount === 4", "all four real API message rows are retained");
        var order = await EvalAsync<string[]>("[...document.querySelectorAll('#messages > article')].map(node => node.dataset.messageId)");
        Check(order.SequenceEqual(expectedOrder), "real prior output stays above guidance and the new reply follows guidance");
        string previousSelector = $"article[data-message-id=\"{previousReplyId}\"]";
        string finalSelector = $"article[data-message-id=\"{currentReplyId}\"]";
        await EvalAsync<bool>($"(() => {{ document.querySelector({JsonSerializer.Serialize(previousSelector)}).scrollIntoView({{block:'start'}}); return true; }})()");
        await WaitAsync($"JSON.stringify([...document.querySelectorAll({JsonSerializer.Serialize(previousSelector + " .message-body")})].filter(body => body._sourceHtml).map(body => body._sourceHtml)) === JSON.stringify({JsonSerializer.Serialize(previousHtml)})",
            "the native DOM retains all actual prior Markdown content without a blank placeholder");
        await EvalAsync<bool>($"(() => {{ document.querySelector({JsonSerializer.Serialize(finalSelector)}).scrollIntoView({{block:'start'}}); return true; }})()");
        await WaitAsync($"document.querySelector({JsonSerializer.Serialize(finalSelector + " .final-answer .message-body")})?._sourceHtml === {JsonSerializer.Serialize(finalHtml)}",
            "the full real final Markdown renders after guidance");
        Check(await EvalAsync<bool>($"document.querySelector({JsonSerializer.Serialize(finalSelector)}).textContent.trim().length > 0 && window.transcriptState().liveElapsedCount === 0"),
            "the new final answer is visible and generation has stopped");
    }

    private async Task CheckSteeringReplyDomAsync(Guid original, Guid replaced, Guid guidance, Guid final)
    {
        Check(await EvalAsync<bool>($"document.querySelector('article[data-message-id=\"{replaced}\"]') !== null && __transcriptSmoke.bodyText().includes('SUPERSEDED_REPLY')"),
            "the stopped prior reply remains rendered with its already-produced content");
        var order = await EvalAsync<string[]>("[...document.querySelectorAll('#messages > article')].map(node => node.dataset.messageId)");
        Check(order.SequenceEqual(new[] { original.ToString(), replaced.ToString(), guidance.ToString(), final.ToString() }),
            "prior output stays above guidance and the new final reply follows guidance");
        Check(await EvalAsync<bool>($$"""
            (() => {
              const article = document.querySelector('article[data-message-id="{{final}}"]');
              return article?.querySelector('h1')?.textContent === '最终交付'
                && article.querySelector('strong')?.textContent === '成本比较'
                && article.querySelectorAll('table tbody tr').length === 1
                && article.querySelector('pre code')?.textContent.trim() === 'FINAL_ARTIFACT_READY';
            })()
            """), "the current final answer renders its actual Markdown heading, emphasis, table and code");
    }
}
