using System.Diagnostics;
using KYNXA_Desktop.Controls;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;
using Windows.Storage.Streams;

namespace KYNXA_Desktop;

public partial class App
{
    private async Task CheckPromptSendAsync(bool queueOnly = false)
    {
        _gateway.EnablePromptSends = true;
        Call("SetPrimaryMode", true, true);
        var chat = _shell.ViewModel.RecentConversations.Single(item => item.Id == _gateway.StandaloneChatId);
        Call("SelectStandaloneChat", chat);
        await WaitAsync(() => !(bool)Call("HasQueuedTaskActivity", chat.Id)!,
            "background task observation confirms that the saved chat is idle");
        await SettleAsync();
        if (queueOnly) { await CheckQueueWaitsForReplyAsync(); return; }
        var firstReply = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        _gateway.PromptReplyGate = firstReply;
        const string first = "Immediate click send";
        NativeUi.SetText(Prompt, first);
        var clicked = Stopwatch.StartNew();
        NativeUi.Invoke(Element<Button>("SendButton"));
        await WaitAsync(() => _shell.ActiveMessages.Any(row => row.Message.Role == "user" && row.Message.Content == first),
            "the user message appears before the pending model reply");
        Check(Prompt.Text == string.Empty && _gateway.PausedReads == 0,
            "idle send clears the composer and presents the user without querying paused roots or selecting a task");
        var transcript = Element<ConversationTranscript>("ConversationMessages");
        await WaitForPromptDomAsync(transcript, first);
        File.AppendAllText(ResultPath, $"Local message and DOM before model reply: {clicked.ElapsedMilliseconds} ms\n");
        NativeUi.SetText(Prompt, "Draft typed during connection preparation");
        _gateway.PromptReplyGate = null;
        firstReply.TrySetResult();
        await WaitAsync(() => _gateway.StreamRequests == 1 && LastAssistantIsComplete(), "the first synthetic reply completes");
        Check(Prompt.Text == "Draft typed during connection preparation",
            "finishing preparation does not erase a newer composer draft");

        await ActivateOwnedShortcutWindowAsync();
        NativeUi.SetText(Prompt, "Enter sends once");
        bool focused = Prompt.Focus(FocusState.Programmatic);
        await WaitAsync(() => ReferenceEquals(FocusManager.GetFocusedElement(_shell.XamlRoot), Prompt),
            "the real composer owns native focus before Enter input");
        await Task.Delay(60);
        object? composingBeforeEnter = typeof(Views.ShellPage).GetField("_promptCompositionActive", PrivateInstance)!.GetValue(_shell);
        object? suppressBeforeEnter = typeof(Views.ShellPage).GetField("_suppressComposingEnter", PrivateInstance)!.GetValue(_shell);
        int previewEnters = 0;
        KeyEventHandler observeEnter = (_, args) => { if (args.Key == Windows.System.VirtualKey.Enter) previewEnters++; };
        Prompt.AddHandler(UIElement.PreviewKeyDownEvent, observeEnter, handledEventsToo: true);
        try
        {
            DispatchShortcut(0x0D, control: false);
            await WaitAsync(() => _gateway.StreamRequests == 2 && LastAssistantIsComplete(), "Enter dispatches the next reply");
        }
        catch (TimeoutException error)
        {
            throw new TimeoutException("Enter dispatches the next reply: " + System.Text.Json.JsonSerializer.Serialize(new
            {
                Prompt.Text, _gateway.StreamRequests, Messages = _shell.ActiveMessages.Count, FocusRequested = focused,
                ComposingBeforeEnter = composingBeforeEnter, SuppressBeforeEnter = suppressBeforeEnter,
                PreviewEnters = previewEnters, FocusedElement = FocusManager.GetFocusedElement(_shell.XamlRoot)?.GetType().FullName,
                FocusState = Prompt.FocusState.ToString(),
                Composing = typeof(Views.ShellPage).GetField("_promptCompositionActive", PrivateInstance)!.GetValue(_shell),
                SuppressEnter = typeof(Views.ShellPage).GetField("_suppressComposingEnter", PrivateInstance)!.GetValue(_shell)
            }), error);
        }
        finally { Prompt.RemoveHandler(UIElement.PreviewKeyDownEvent, observeEnter); }
        Check(Prompt.Text == string.Empty && _shell.ActiveMessages.Count(row => row.Message.Role == "user"
            && row.Message.Content == "Enter sends once") == 1 && _gateway.PausedReads == 0,
            "Enter adds one message without a leftover newline or obsolete paused-task query");
        await WaitForPromptDomAsync(transcript, "Fixture reply: Enter sends once");

        NativeUi.SetText(Prompt, "Shift retains newline");
        Prompt.SelectionStart = Prompt.Text.Length;
        Prompt.Focus(FocusState.Programmatic);
        DispatchShortcut(0x0D, control: false, shift: true);
        await SettleAsync();
        Check((Prompt.Text.Contains('\r') || Prompt.Text.Contains('\n')) && _gateway.StreamRequests == 2,
            "Shift+Enter inserts a normal newline without sending");

        // Exercise the two IME event-order guards with native Enter; the OS input method itself stays unchanged.
        // 用真实 Enter 检查两种输入法事件顺序保护，不更改本机输入法配置。
        NativeUi.SetText(Prompt, "中文输入确认");
        typeof(Views.ShellPage).GetField("_promptCompositionActive", PrivateInstance)!.SetValue(_shell, true);
        DispatchShortcut(0x0D, control: false);
        await SettleAsync();
        typeof(Views.ShellPage).GetField("_promptCompositionActive", PrivateInstance)!.SetValue(_shell, false);
        Check(_gateway.StreamRequests == 2, "Enter during IME composition does not send a message");
        NativeUi.SetText(Prompt, "中文已确认");
        typeof(Views.ShellPage).GetField("_suppressComposingEnter", PrivateInstance)!.SetValue(_shell, true);
        DispatchShortcut(0x0D, control: false);
        await SettleAsync();
        Check(Prompt.Text == "中文已确认" && _gateway.StreamRequests == 2,
            "the IME confirming Enter neither sends nor leaves a stray newline");
        NativeUi.SetText(Prompt, string.Empty);
        await CheckClipboardAttachmentSendAsync();
        await CheckQueueWaitsForReplyAsync();
    }

    private async Task CheckQueueWaitsForReplyAsync()
    {
        int priorRequests = _gateway.StreamRequests;
        var gate = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        _gateway.PromptReplyGate = gate;
        try
        {
            NativeUi.SetText(Prompt, "PARENT_REPLY_HELD_FOR_QUEUE");
            await WaitAsync(() => Element<Button>("SendButton").IsEnabled,
                "the settled composer enables the next ordinary send");
            NativeUi.Invoke(Element<Button>("SendButton"));
            await WaitAsync(() => _gateway.StreamRequests == priorRequests + 1, "the parent reply is executing");
            NativeUi.SetText(Prompt, "SEND_ONLY_AFTER_PARENT_COMPLETES");
            NativeUi.Invoke(Element<Button>("SendButton"));
            await Task.Delay(80);
            Check(_gateway.StreamRequests == priorRequests + 1
                && Element<ConversationPromptQueue>("PromptQueue").Visibility == Visibility.Visible
                && !_shell.ActiveMessages.Any(row => row.Message.Content == "SEND_ONLY_AFTER_PARENT_COMPLETES"),
                "queued input remains a card and makes no request while the current reply is running");
            _gateway.PromptReplyGate = null;
            gate.TrySetResult();
            await WaitAsync(() => _gateway.StreamRequests == priorRequests + 2 && LastAssistantIsComplete(),
                "the queued message dispatches after the parent reply actually completes");
            Check(_shell.ActiveMessages.Count(row => row.Message.Role == "user"
                && row.Message.Content == "SEND_ONLY_AFTER_PARENT_COMPLETES") == 1
                && Element<ConversationPromptQueue>("PromptQueue").Visibility == Visibility.Collapsed,
                "queue completion retains one user message and removes its sent card");
        }
        finally { _gateway.PromptReplyGate = null; gate.TrySetResult(); }
    }

    private async Task CheckClipboardAttachmentSendAsync()
    {
        _gateway.EnableAttachmentImports = true;
        string source = Path.Combine(_directory, "clipboard-source.png");
        await File.WriteAllBytesAsync(source, Convert.FromBase64String(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jJesAAAAASUVORK5CYII="));
        var package = new DataPackage();
        package.SetBitmap(RandomAccessStreamReference.CreateFromFile(await StorageFile.GetFileFromPathAsync(source)));
        Clipboard.SetContent(package);
        try
        {
            Check(await (Task<bool>)Call("TryPasteAttachmentsAsync")!, "production composer accepts clipboard pixels");
            var drafts = (Dictionary<string, List<Services.MessageAttachmentDraft>>)typeof(Views.ShellPage)
                .GetField("_attachmentDrafts", PrivateInstance)!.GetValue(_shell)!;
            var draft = drafts.Values.SelectMany(files => files).Single();
            Check(draft.IsTemporary && File.Exists(draft.Path)
                && Path.GetDirectoryName(draft.Path) == Services.StoragePaths.AttachmentDraftDirectory,
                "clipboard PNG is staged in the operating system temporary directory");
            NativeUi.Invoke(Element<Button>("SendButton"));
            await WaitAsync(() => _gateway.AttachmentImports == 1 && !(bool)Call("IsReplyInProgress", ActiveId)!,
                "a failed import completes without a model request");
            var failed = _shell.ActiveMessages.Last().Message;
            var user = _shell.ActiveMessages.Last(row => row.Message.Role == "user").Message;
            Check(failed.Status == "error" && failed.Content == string.Empty && _gateway.StreamRequests == 2
                && File.Exists(draft.Path), "import failure is an error state and retains the screenshot for retry");
            // Historical descriptors default to ready; that must never bypass the outstanding import.
            // 历史描述符默认 ready，不能因此跳过仍未完成的附件导入。
            user.Attachments = user.Attachments.Select(file => file with { Status = "ready" }).ToList();
            Call("Transcript_RetryRequested", null, failed.Id);
            await WaitAsync(() => _gateway.AttachmentImports == 2 && _gateway.StreamRequests == 3 && LastAssistantIsComplete(),
                "retry imports the same screenshot before dispatching the model request");
            Check(!File.Exists(draft.Path) && user.Attachments.Single().Sha256 == _gateway.ImportedAttachment!.Sha256
                && _shell.ActiveMessages.Count(row => row.Message.Id == user.Id) == 1,
                "a real receipt updates the attachment, removes the temporary PNG and keeps one original user message");
            await SettleAsync();
            if (_gateway.Failure is { } error) throw new InvalidOperationException("Unexpected attachment fixture request", error);
        }
        finally { Clipboard.Clear(); }
    }

    private bool LastAssistantIsComplete() => _shell.ActiveMessages.LastOrDefault() is { Message.Role: "assistant", Message.Status: "completed" }
        && !(bool)Call("IsReplyInProgress", ActiveId)!;

    private async Task WaitForPromptDomAsync(ConversationTranscript transcript, string text)
    {
        DateTime deadline = DateTime.UtcNow.AddSeconds(10);
        string source = "document.body.innerText.includes(" + System.Text.Json.JsonSerializer.Serialize(text) + ")";
        while (await transcript.Browser.ExecuteScriptAsync(source) != "true")
        {
            if (DateTime.UtcNow > deadline) throw new TimeoutException("Accepted prompt did not reach the production transcript DOM.");
            await Task.Delay(30);
        }
        Check(true, "the production transcript renders: " + text);
    }
}
