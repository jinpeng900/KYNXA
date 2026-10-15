using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.ApplicationModel.DataTransfer;
using Windows.Graphics.Imaging;
using Windows.Storage;
using Windows.Storage.Pickers;
using Windows.Storage.Streams;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private readonly MessageAttachmentsApiClient _attachmentsApiClient = new();
    private readonly Dictionary<string, List<MessageAttachmentDraft>> _attachmentDrafts = [];
    private readonly Dictionary<Guid, MessageAttachmentDraft[]> _messageAttachmentDrafts = [];

    private string AttachmentDraftKey => ActiveChatId?.ToString("D")
        ?? (ViewModel.IsChatMode ? "chat" : $"work:{_selectedWorkProjectId}");
    private List<MessageAttachmentDraft> AttachmentDraftsFor(string key)
    {
        if (!_attachmentDrafts.TryGetValue(key, out var files)) _attachmentDrafts[key] = files = [];
        return files;
    }
    private bool HasAttachmentDrafts => AttachmentDraftsFor(AttachmentDraftKey).Count > 0;

    private void InitializeMessageAttachments()
    {
        ComposerAttachments.RemoveRequested += (_, file) =>
        {
            AttachmentDraftsFor(AttachmentDraftKey).Remove(file);
            DeleteTemporaryAttachment(file);
            RefreshAttachmentDrafts();
        };
        ComposerHost.AllowDrop = true;
        ComposerHost.DragOver += ComposerAttachments_DragOver;
        ComposerHost.Drop += ComposerAttachments_Drop;
    }

    private void RefreshAttachmentDrafts()
    {
        if (ComposerAttachments is null) return;
        ComposerAttachments.ShowFiles(AttachmentDraftsFor(AttachmentDraftKey));
        PromptTextBox_TextChanged(PromptTextBox, null!);
    }

    private MessageAttachmentDraft[] TakeAttachmentDrafts()
    {
        var files = AttachmentDraftsFor(AttachmentDraftKey);
        var selected = files.ToArray();
        files.Clear();
        RefreshAttachmentDrafts();
        return selected;
    }

    private async void ComposerAddButton_Click(object sender, RoutedEventArgs args)
    {
        string draftKey = AttachmentDraftKey;
        try
        {
            var picker = new FileOpenPicker { SuggestedStartLocation = PickerLocationId.DocumentsLibrary };
            picker.FileTypeFilter.Add("*");
            WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(App.Window));
            var files = await picker.PickMultipleFilesAsync();
            await AddAttachmentFilesAsync(files, draftKey);
        }
        catch (Exception error) when (error is System.Runtime.InteropServices.COMException or IOException or UnauthorizedAccessException or InvalidOperationException)
        { if (!_chatClosing) ShowActionFeedback(UiText.Get("附件添加失败，请重试。")); }
    }

    private async Task AddAttachmentFilesAsync(IEnumerable<StorageFile> files, string draftKey, bool temporary = false)
    {
        var drafts = AttachmentDraftsFor(draftKey);
        foreach (var file in files)
        {
            if (_chatClosing) return;
            if (drafts.Any(item => string.Equals(item.Path, file.Path, StringComparison.OrdinalIgnoreCase))) continue;
            var properties = await file.GetBasicPropertiesAsync();
            var draft = new MessageAttachmentDraft(Guid.NewGuid(), file.Path, file.Name,
                string.IsNullOrEmpty(file.ContentType) ? ContentTypeFor(file.FileType) : file.ContentType,
                checked((long)properties.Size), temporary);
            drafts.Add(draft);
            if (draftKey == AttachmentDraftKey) RefreshAttachmentDrafts();
            if (draft.ContentType.StartsWith("image/", StringComparison.OrdinalIgnoreCase))
            {
                try
                {
                    using var thumbnail = await file.GetThumbnailAsync(Windows.Storage.FileProperties.ThumbnailMode.SingleItem, 180);
                    if (thumbnail is not null)
                    {
                        var bitmap = new BitmapImage { DecodePixelWidth = 180 };
                        await bitmap.SetSourceAsync(thumbnail);
                        draft.Thumbnail = bitmap;
                        thumbnail.Seek(0);
                        using var reader = new DataReader(thumbnail.GetInputStreamAt(0));
                        await reader.LoadAsync((uint)thumbnail.Size);
                        byte[] bytes = new byte[checked((int)thumbnail.Size)];
                        reader.ReadBytes(bytes);
                        draft.ThumbnailDataUrl = "data:" + thumbnail.ContentType + ";base64," + Convert.ToBase64String(bytes);
                    }
                }
                catch (Exception error) when (error is System.Runtime.InteropServices.COMException or IOException or InvalidOperationException) { }
            }
        }
        if (draftKey == AttachmentDraftKey) RefreshAttachmentDrafts();
    }

    private static string ContentTypeFor(string extension) => extension.ToLowerInvariant() switch
    {
        ".md" or ".markdown" => "text/markdown", ".txt" => "text/plain", ".pdf" => "application/pdf",
        ".docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ".pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        ".xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        ".doc" => "application/msword", ".ppt" => "application/vnd.ms-powerpoint", ".csv" => "text/csv",
        ".png" => "image/png", ".jpg" or ".jpeg" => "image/jpeg", ".gif" => "image/gif",
        ".webp" => "image/webp", ".bmp" => "image/bmp", ".svg" => "image/svg+xml", _ => "application/octet-stream"
    };

    private void ComposerAttachments_DragOver(object sender, DragEventArgs args)
    {
        if (!args.DataView.Contains(StandardDataFormats.StorageItems)) return;
        args.AcceptedOperation = DataPackageOperation.Copy;
        args.Handled = true;
    }

    private async void ComposerAttachments_Drop(object sender, DragEventArgs args)
    {
        if (!args.DataView.Contains(StandardDataFormats.StorageItems)) return;
        args.Handled = true;
        string key = AttachmentDraftKey;
        var deferral = args.GetDeferral();
        try { await AddAttachmentFilesAsync((await args.DataView.GetStorageItemsAsync()).OfType<StorageFile>(), key); }
        catch (Exception error) when (error is System.Runtime.InteropServices.COMException or IOException or UnauthorizedAccessException)
        { if (!_chatClosing) ShowActionFeedback(UiText.Get("附件添加失败，请重试。")); }
        finally { deferral.Complete(); }
    }

    private async Task<bool> TryPasteAttachmentsAsync()
    {
        var content = Clipboard.GetContent();
        string key = AttachmentDraftKey;
        if (content.Contains(StandardDataFormats.StorageItems))
        {
            await AddAttachmentFilesAsync((await content.GetStorageItemsAsync()).OfType<StorageFile>(), key);
            return true;
        }
        if (!content.Contains(StandardDataFormats.Bitmap)) return false;
        // Clipboard pixels need a temporary file; only Send imports a durable gateway snapshot.
        // 剪贴板像素暂存为文件，点击发送后才由网关接收为持久附件。
        var folder = await ApplicationData.Current.TemporaryFolder.CreateFolderAsync("AttachmentDrafts", CreationCollisionOption.OpenIfExists);
        var file = await folder.CreateFileAsync("截图-" + DateTime.Now.ToString("yyyyMMdd-HHmmss") + ".png", CreationCollisionOption.GenerateUniqueName);
        using (var source = await (await content.GetBitmapAsync()).OpenReadAsync())
        using (var output = await file.OpenAsync(FileAccessMode.ReadWrite))
        {
            var decoder = await BitmapDecoder.CreateAsync(source);
            using var bitmap = await decoder.GetSoftwareBitmapAsync();
            var encoder = await BitmapEncoder.CreateAsync(BitmapEncoder.PngEncoderId, output);
            encoder.SetSoftwareBitmap(bitmap);
            await encoder.FlushAsync();
        }
        await AddAttachmentFilesAsync([file], key, temporary: true);
        return true;
    }

    private static bool ClipboardContainsAttachments()
    {
        try
        {
            var content = Clipboard.GetContent();
            return content.Contains(StandardDataFormats.StorageItems) || content.Contains(StandardDataFormats.Bitmap);
        }
        catch (System.Runtime.InteropServices.COMException) { return false; }
    }

    private async Task PrepareMessageAttachmentsAsync(Guid conversation, ChatMessageState user, CancellationToken cancellationToken)
    {
        if (!_messageAttachmentDrafts.TryGetValue(user.Id, out var files) || files.Length == 0) return;
        if (user.Attachments.Count > 0 && user.Attachments.All(file => file.Status != "pending")) return;
        var accepted = await _attachmentsApiClient.ImportAsync(conversation, user.Id, files, cancellationToken);
        // Preserve only in-memory thumbnails while replacing provisional metadata with real receipts.
        // 用真实接收回执替换临时元信息，缩略图仅保留在本机展示状态中。
        user.Attachments = accepted.Select(file => file with
            { ThumbnailDataUrl = user.Attachments.FirstOrDefault(previous => previous.Id == file.Id)?.ThumbnailDataUrl }).ToList();
        if (ActiveChatId == conversation) UpdateConversationPresentation(openAtBottom: false);
        if (accepted.Any(file => file.Status == "pending")) throw new InvalidDataException(UiText.Get("附件仍在接收，请重试。"));
        _messageAttachmentDrafts.Remove(user.Id);
        foreach (var file in files) DeleteTemporaryAttachment(file);
    }

    private static void DeleteTemporaryAttachment(MessageAttachmentDraft file)
    {
        if (!file.IsTemporary) return;
        try { File.Delete(file.Path); }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException) { }
    }
}
