using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using Windows.ApplicationModel.DataTransfer;

namespace TranscriptUiSmoke;

public partial class App
{
    private async Task CheckMessageAttachmentsAsync()
    {
        foreach (string resource in new[] { "Colors", "Dimensions", "Controls" })
            Resources.MergedDictionaries.Add(new Microsoft.UI.Xaml.ResourceDictionary { Source = new Uri($"ms-appx:///Styles/{resource}.xaml") });
        MessageAttachmentDraft[] drafts = [
            new(Guid.NewGuid(), "fixture/report.md", "KYNXA-你好.md", "text/markdown", 128),
            new(Guid.NewGuid(), "fixture/report.docx", "高代实践作业.docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document", 512),
            new(Guid.NewGuid(), "fixture/image.png", "屏幕截图.png", "image/png", 68)
        ];
        var composerFiles = new ConversationAttachmentDrafts();
        composerFiles.ShowFiles(drafts);
        Check(composerFiles.Visibility == Microsoft.UI.Xaml.Visibility.Visible
            && ((Microsoft.UI.Xaml.Controls.StackPanel)((Microsoft.UI.Xaml.Controls.ScrollViewer)composerFiles.Children[0]).Content).Children.Count == 3,
            "the composer creates one removable card per selected file");
        var queued = new QueuedPrompt(Guid.NewGuid(), "", null, null, ChatPermissionModes.Ask, drafts);
        Check(queued.DisplayText.Contains("高代实践作业.docx", StringComparison.Ordinal) && queued.Attachments.Count == 3,
            "an attachment-only queued message retains its files and readable queue label");
        await using var fixture = new AttachmentImageFixture();
        ModelGatewayService.FixtureAddress = fixture.Address;
        var conversationId = Guid.NewGuid();
        var imageId = Guid.NewGuid();
        var pendingId = Guid.NewGuid();
        const string request = "分析这些附件 **保留原样**";
        var message = new ChatMessageState
        {
            Role = "user", Content = request,
            Attachments = [
                new(imageId, "屏幕截图.png", "image/png", 68, "ready"),
                new(Guid.NewGuid(), "高代实践作业.docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document", 512, "ready"),
                new(Guid.NewGuid(), "KYNXA-你好.md", "text/markdown", 128, "ready"),
                new(pendingId, "正在发送.png", "image/png", 68, "pending") { ThumbnailDataUrl = AttachmentImageFixture.ImageDataUrl }
            ]
        };
        var old = Message(conversationId, "assistant", "OLD_MESSAGE_STILL_READABLE");
        var view = new ConversationMessageViewModel(conversationId, message);
        _transcript.ShowConversation(conversationId, [old, view]);
        try
        {
            await WaitAsync("document.querySelectorAll('.message-attachment').length === 4 && [...document.querySelectorAll('.attachment-image img')].every(image => image.complete && image.naturalWidth > 0)",
                "sent images render from the authorized native endpoint and a transient local thumbnail");
        }
        catch (InvalidOperationException error)
        {
            string state = await EvalAsync<string>("JSON.stringify({cards:document.querySelectorAll('.message-attachment').length,images:[...document.querySelectorAll('.attachment-image img')].map(image=>({src:image.src,complete:image.complete,width:image.naturalWidth})),text:document.body.textContent})");
            throw new InvalidOperationException(error.Message + "; requests=" + fixture.Requests + "; state=" + state);
        }
        Check(fixture.Requests == 1, "only the durable image reads the attachment endpoint; draft pixels stay local");
        Check(await EvalAsync<bool>("(() => { const content=document.querySelector('.has-attachments'), files=content.querySelector('.message-attachments'), body=content.querySelector('.message-body'); return files.compareDocumentPosition(body) === Node.DOCUMENT_POSITION_FOLLOWING && files.getBoundingClientRect().bottom <= body.getBoundingClientRect().top && body.textContent === '分析这些附件 **保留原样**'; })()"),
            "attachments are above the unchanged literal user text bubble");
        Check(await EvalAsync<bool>("document.querySelectorAll('.attachment-document').length === 1 && document.querySelector('.attachment-document .attachment-name').textContent === '高代实践作业.docx' && document.querySelector('.attachment-file-pill .attachment-name').textContent === 'KYNXA-你好.md'"),
            "Office files use document cards while Markdown uses a compact file pill");
        Check(await EvalAsync<bool>("document.querySelector('.message-body').textContent === 'OLD_MESSAGE_STILL_READABLE' && !document.querySelector('article.assistant .has-attachments')"),
            "old messages without attachments retain their original display");
        ConversationAttachmentRequest? clicked = null;
        void OnAttachment(object? sender, ConversationAttachmentRequest value) => clicked = value;
        _transcript.AttachmentRequested += OnAttachment;
        try
        {
            await EvalAsync<bool>("(() => { document.querySelector('.attachment-document').click(); return true; })()");
            await Task.Delay(100);
            Check(clicked?.ConversationId == conversationId && clicked.MessageId == message.Id && clicked.Attachment.Name == "高代实践作业.docx",
                "a document click carries validated conversation, message and attachment identity to the workspace");
            await EvalAsync<bool>("(() => { document.querySelector('article.user .copy-message').click(); return true; })()");
            await WaitAsync("document.querySelector('article.user .copy-message').dataset.copyState === 'success'", "message copy is acknowledged");
            Check(await Clipboard.GetContent().GetTextAsync() == request, "copy contains user text without attachment paths, thumbnails or UI labels");
            message.Attachments[3] = message.Attachments[3] with { Status = "ready", ThumbnailDataUrl = null };
            view.Refresh();
            await WaitAsync("document.querySelectorAll('.attachment-image:not(:disabled)').length === 2 && [...document.querySelectorAll('.attachment-image img')].every(image => image.complete && image.naturalWidth > 0)",
                "upload completion updates the existing attachment without replacing the user message");
            var stored = JsonSerializer.Serialize(message);
            Check(!stored.Contains("base64", StringComparison.Ordinal), "the formal message contains stable references and no draft image bytes");
            _transcript.ShowConversation(Guid.NewGuid(), []);
            await WaitAsync("document.querySelectorAll('article').length === 0", "changing conversations removes attachment views");
            var restored = JsonSerializer.Deserialize<ChatMessageState>(stored)!;
            _transcript.ShowConversation(conversationId, [new(conversationId, restored)]);
            await WaitAsync("document.querySelectorAll('.message-attachment').length === 4 && [...document.querySelectorAll('.attachment-image img')].every(image => image.complete && image.naturalWidth > 0)",
                "history reload restores attachment cards and image content by stable IDs");
            _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(480, 850));
            await Task.Delay(150);
            Check(await EvalAsync<bool>("document.documentElement.scrollWidth <= innerWidth && [...document.querySelectorAll('.message-attachment')].every(node => node.getBoundingClientRect().right <= innerWidth)"),
                "attachment cards wrap inside a narrow conversation");
            using var screenshot = File.Create(Path.Combine(Path.GetTempPath(), "kynxa-message-attachments.png"));
            await _transcript.Browser.CoreWebView2.CapturePreviewAsync(Microsoft.Web.WebView2.Core.CoreWebView2CapturePreviewImageFormat.Png, screenshot.AsRandomAccessStream());
        }
        finally { _transcript.AttachmentRequested -= OnAttachment; }
    }

    // A loopback image fixture exercises the real native resource interception without user data or a model request.
    // 本地图片夹具验证真实的原生资源拦截，不读取用户数据或派发模型请求。
    private sealed class AttachmentImageFixture : IAsyncDisposable
    {
        public const string ImageDataUrl = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a1FoAAAAASUVORK5CYII=";
        private readonly TcpListener _listener = new(IPAddress.Loopback, 0);
        private readonly CancellationTokenSource _cancellation = new();
        private readonly Task _server;
        private int _requests;
        public Uri Address { get; }
        public int Requests => Volatile.Read(ref _requests);
        public AttachmentImageFixture()
        {
            _listener.Start();
            Address = new Uri($"http://127.0.0.1:{((IPEndPoint)_listener.LocalEndpoint).Port}");
            _server = ServeAsync();
        }
        private async Task ServeAsync()
        {
            try
            {
                while (!_cancellation.IsCancellationRequested)
                {
                    using var client = await _listener.AcceptTcpClientAsync(_cancellation.Token);
                    await using var stream = client.GetStream();
                    using var reader = new StreamReader(stream, Encoding.ASCII, leaveOpen: true);
                    string? request = await reader.ReadLineAsync(_cancellation.Token);
                    while (await reader.ReadLineAsync(_cancellation.Token) is { Length: > 0 }) { }
                    bool valid = request?.Contains("/attachments/", StringComparison.Ordinal) == true
                        && request.Contains("/content?messageId=", StringComparison.Ordinal);
                    if (valid) Interlocked.Increment(ref _requests);
                    byte[] bytes = valid ? Convert.FromBase64String(ImageDataUrl.Split(',')[1]) : [];
                    byte[] header = Encoding.ASCII.GetBytes($"HTTP/1.1 {(valid ? "200 OK" : "404 Not Found")}\r\nContent-Type: image/png\r\nContent-Length: {bytes.Length}\r\nConnection: close\r\n\r\n");
                    await stream.WriteAsync(header, _cancellation.Token);
                    await stream.WriteAsync(bytes, _cancellation.Token);
                }
            }
            catch (Exception error) when (error is OperationCanceledException or SocketException or IOException) { }
        }
        public async ValueTask DisposeAsync()
        {
            _cancellation.Cancel(); _listener.Stop();
            await _server; _cancellation.Dispose();
        }
    }
}
