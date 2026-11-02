using System.Text.Json;
using KYNXA_Desktop.Services;
using Windows.Storage.Pickers;

namespace KYNXA_Desktop.Controls;

public sealed partial class ConversationDocumentPanel
{
    private bool _trainingPickerPending;

    private async Task<JsonElement> PickTrainingSourceAsync(string action, JsonElement payload, DocumentWork work)
    {
        if (_trainingPickerPending) throw new InvalidOperationException("文件选择器已经打开。");
        _trainingPickerPending = true;
        try
        {
            string role = JsonString(payload, "role", 20) ?? "data";
            if (role is not ("data" or "model" or "python")) throw new InvalidDataException("训练来源类型无效。");
            object source;
            string operation;
            if (action == "training.folder.pick")
            {
                var picker = new FolderPicker { SuggestedStartLocation = PickerLocationId.DocumentsLibrary };
                picker.FileTypeFilter.Add("*");
                WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(App.Window));
                var folder = await picker.PickSingleFolderAsync();
                if (folder is null || _disposed || !IsLive(work)) return JsonSerializer.SerializeToElement(new { cancelled = true, sources = Array.Empty<object>() });
                operation = role == "model" ? "training.models.register" : "training.sources.import";
                source = role == "model" ? new { path = folder.Path } : (object)new { directory = folder.Path };
            }
            else
            {
                var picker = new FileOpenPicker { SuggestedStartLocation = PickerLocationId.DocumentsLibrary };
                picker.FileTypeFilter.Add(role == "python" ? ".exe" : "*");
                WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(App.Window));
                var files = await picker.PickMultipleFilesAsync();
                if (files.Count == 0 || _disposed || !IsLive(work)) return JsonSerializer.SerializeToElement(new { cancelled = true, sources = Array.Empty<object>() });
                if (role == "python")
                {
                    if (files.Count != 1) throw new InvalidDataException("请选择一个Python解释器。");
                    operation = "training.environment.configure"; source = new { path = files[0].Path };
                }
                else
                {
                    operation = "training.sources.import"; source = new { sources = files.Select(f => new { path = f.Path }).ToArray() };
                }
            }
            using var api = new TrainingSourcesApiClient();
            return await api.ImportAsync(work.ConversationId, work.MountId, operation, source, work.Cancellation.Token);
        }
        catch (Exception e) when (e is System.Runtime.InteropServices.COMException or UnauthorizedAccessException or IOException)
        { throw new InvalidOperationException("训练来源选择或导入失败，请检查文件是否可访问。", e); }
        finally { _trainingPickerPending = false; }
    }
}
