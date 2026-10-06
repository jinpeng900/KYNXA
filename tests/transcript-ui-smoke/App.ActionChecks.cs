using KYNXA_Desktop.ViewModels;
using Windows.ApplicationModel.DataTransfer;

namespace TranscriptUiSmoke;

public partial class App
{
    private async Task CheckTranscriptActionsAsync()
    {
        var chat = Guid.NewGuid();
        const string copiedMarkdown = "COPY_ACK_FIXTURE\n\n```cs\n    int value = 1;\n```\n\n$\\alpha$";
        var rows = new List<ConversationMessageViewModel>
        {
            Message(chat, "user", "JUMP_SELECTION_FIXTURE " + string.Join("\n", Enumerable.Repeat("Read earlier content.", 80))),
            Message(chat, "assistant", copiedMarkdown)
        };
        var shortChat = Guid.NewGuid();
        _transcript.ShowConversation(shortChat, [Message(shortChat, "assistant", "SHORT_NAVIGATION_FIXTURE")]);
        await WaitAsync("__transcriptSmoke.bodyText().includes('SHORT_NAVIGATION_FIXTURE')", "short navigation fixture replaces earlier scrollable history");
        await EvalAsync<bool>($$"""
            (() => {
              const listener = event => {
                if (event.data?.type !== 'render' || event.data.conversationId !== '{{chat}}') return;
                window.chrome.webview.removeEventListener('message', listener);
                window.__navigationScrollProbe = { beforeBottomFrame: __transcriptSmoke.bottomDistance() > 36 };
                // Deliver the old-DOM scroll notification after the replacement but before the queued bottom RAF.
                // 在新 DOM 替换后、已排队的底部 RAF 前送达旧 DOM 产生的滚动通知。
                window.dispatchEvent(new Event('scroll'));
                window.__navigationScrollProbe.following = window.transcriptState().following;
              };
              window.chrome.webview.addEventListener('message', listener);
              return true;
            })()
            """);
        _transcript.ShowConversation(chat, rows);
        await WaitAsync("__transcriptSmoke.bodyText().includes('COPY_ACK_FIXTURE') && __transcriptSmoke.bottomDistance() < 4", "action fixture opens at bottom");
        Check(await EvalAsync<bool>("window.__navigationScrollProbe?.beforeBottomFrame === true && window.__navigationScrollProbe.following === true"),
            "late old-DOM scroll notification cannot cancel a new conversation's first bottom frame");
        Check(await EvalAsync<bool>("document.getElementById('jump-to-latest').hidden"), "jump control stays hidden at bottom");

        var feedback = new TaskCompletionSource<string>(TaskCreationOptions.RunContinuationsAsynchronously);
        EventHandler<string> callback = (_, text) => feedback.TrySetResult(text);
        _transcript.ActionFeedbackRequested += callback;
        try
        {
            Check(await EvalAsync<bool>("(() => { const copy = document.querySelector('[data-role=assistant] .copy-message'); copy.click(); return !copy.dataset.copyState; })()"),
                "copy click does not report success before native acknowledgement");
            string result = await feedback.Task.WaitAsync(TimeSpan.FromSeconds(5));
            Check(result == KYNXA_Desktop.Services.UiText.Get("已复制"), "copy action reports the actual native clipboard result");
            await WaitAsync("document.querySelector('[data-role=assistant] .copy-message').dataset.copyState === 'success'", "matching native acknowledgement changes the copy button");
            Check(await Clipboard.GetContent().GetTextAsync() == copiedMarkdown, "native clipboard contains original Markdown, TeX and indentation");
        }
        finally { _transcript.ActionFeedbackRequested -= callback; }

        await EvalAsync<bool>("__transcriptSmoke.scrollUp()");
        await WaitAsync("!document.getElementById('jump-to-latest').hidden && !window.transcriptState().following", "upward reading exposes jump control and pauses following");
        await EvalAsync<bool>("(() => { document.getElementById('jump-to-latest').click(); return true; })()");
        await WaitAsync("document.getElementById('jump-to-latest').hidden && window.transcriptState().following && __transcriptSmoke.bottomDistance() < 4", "explicit jump resumes following at latest content");

        await EvalAsync<bool>("""
            (() => {
              scrollTo({top:0,behavior:'instant'});
              const body = document.querySelector('[data-role=user] .message-body');
              const node = body.firstChild;
              getSelection().setBaseAndExtent(node, 0, node, 22);
              window.__jumpSelection = window.transcriptSelectionText();
              return true;
            })()
            """);
        await WaitAsync("window.transcriptState().selection", "jump selection fixture has an active range");
        rows[1].Message.Content += "\n\nDEFERRED_JUMP_UPDATE";
        rows[1].Refresh();
        await WaitAsync("window.transcriptState().pending", "selected text freezes new transcript content");
        _transcript.JumpToLatest();
        await WaitAsync("__transcriptSmoke.bottomDistance() < 4", "host jump reaches latest displayed content");
        Check(await EvalAsync<bool>("window.transcriptSelectionText() === window.__jumpSelection && window.transcriptState().pending && !window.transcriptState().following"),
            "jump preserves selection and deferred updates without restarting automatic following");
        _transcript.ClearSelection();
        await WaitAsync("__transcriptSmoke.bodyText().includes('DEFERRED_JUMP_UPDATE')", "clearing preserved selection applies the pending update");
    }
}
