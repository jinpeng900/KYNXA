using Windows.ApplicationModel.DataTransfer;

namespace TranscriptUiSmoke;

public partial class App
{
    private const string ClipboardPersistenceSource = "CLIPBOARD_AFTER_EXIT 中文 🧪\n\n\tIndented source\nEND_PERSISTENCE";

    private async Task CheckClipboardPersistenceAsync(bool readAfterExit)
    {
        if (readAfterExit)
        {
            // This process starts only after the copying fixture exits; do not materialize its package early.
            // 本进程只在复制夹具退出后启动；不得提前读取并实体化发送方的数据包。
            var content = Clipboard.GetContent();
            Check(content.Contains(StandardDataFormats.Text), "the clipboard still advertises text after the copying process exits");
            Check(await content.GetTextAsync() == ClipboardPersistenceSource, "a fresh process reads exact Unicode, indentation and blank lines after the copying owner exits");
            _metrics["clipboardPersistencePhase"] = "reader: a new process, after the owner exited; no clipboard write";
            return;
        }

        var conversationId = Guid.NewGuid();
        _transcript.ShowConversation(conversationId, [Message(conversationId, "assistant", "```text\n" + ClipboardPersistenceSource + "\n```")]);
        await WaitAsync("!!document.querySelector('.copy-block')", "the persistence source renders in the production content block");
        await EvalAsync<bool>("(() => {document.querySelector('.copy-block').click();return true;})()");
        await WaitAsync("document.querySelector('.copy-block').dataset.copyState==='success'", "the production native copy bridge acknowledges the persistence source");
        Check(await EvalAsync<string>("document.querySelector('.content-block code').textContent") == ClipboardPersistenceSource,
            "the copied fixture contains the expected source before exit");
        _metrics["clipboardPersistencePhase"] = "writer: production copy receipt, no Clipboard.GetContent or paste before process exit";
    }
}
