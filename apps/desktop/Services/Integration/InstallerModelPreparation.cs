using System.Text.Json;
using System.Text.Json.Nodes;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Submits the user's installer selection to the normal Rust model manager after startup.
/// 启动后把安装器中明确勾选的模型交给正式 Rust 管理器，下载进度仍由模型设置页面展示。
/// </summary>
internal static class InstallerModelPreparation
{
    public static async Task RunAsync()
    {
        string requestPath = Path.Combine(ApplicationPaths.DataRoot, "Installer", "local-model-request.json");
        if (!File.Exists(requestPath)) return;
        JsonObject? request = null;
        try
        {
            request = JsonNode.Parse(await File.ReadAllTextAsync(requestPath).ConfigureAwait(false))?.AsObject();
            if (request is null || request["version"]?.GetValue<int>() != 1 ||
                request["status"]?.GetValue<string>() is "completed" or "paused") return;
            string? modelId = request["modelId"]?.GetValue<string>();
            if (string.IsNullOrWhiteSpace(modelId)) return;
            using var client = new LocalModelsApiClient();
            var snapshot = await client.ReadAsync().ConfigureAwait(false);
            var model = snapshot.Models.FirstOrDefault(item => item.Id == modelId)
                ?? throw new InvalidDataException("The installer-selected local model is not in the model catalog.");
            if (!model.Installed && model.Status != "downloading")
                snapshot = await client.InstallAsync(modelId).ConfigureAwait(false);
            request["status"] = "downloading";
            request.Remove("error");
            await SaveAsync(requestPath, request).ConfigureAwait(false);
            while (true)
            {
                model = snapshot.Models.FirstOrDefault(item => item.Id == modelId);
                if (model is null)
                {
                    // Explicit deletion cancels the installer selection instead of reinstalling it on the next launch.
                    // 用户主动删除即取消安装意向，下次启动不能悄悄重新下载。
                    request["status"] = "paused";
                    await SaveAsync(requestPath, request).ConfigureAwait(false);
                    return;
                }
                if (model.Installed)
                {
                    if (!model.Enabled && request["enableAfterInstall"]?.GetValue<bool>() != false)
                        await client.EnableAsync(modelId, true).ConfigureAwait(false);
                    request["status"] = "completed";
                    await SaveAsync(requestPath, request).ConfigureAwait(false);
                    return;
                }
                if (model.Status is "paused" or "cancelled" || model.Status == "available")
                {
                    request["status"] = "paused";
                    await SaveAsync(requestPath, request).ConfigureAwait(false);
                    return;
                }
                if (model.Status == "failed") throw new IOException(model.Error.ToString());
                await Task.Delay(TimeSpan.FromSeconds(5)).ConfigureAwait(false);
                snapshot = await client.ReadAsync().ConfigureAwait(false);
            }
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or JsonException or InvalidOperationException or HttpRequestException or OperationCanceledException)
        {
            // Keep the accepted intent for the next launch; a failed optional download never blocks chat.
            // 保留已授权安装意向供下次启动接续；可选下载失败不阻塞界面和对话。
            System.Diagnostics.Debug.WriteLine($"Installer-selected model: {error.Message}");
            if (request is null) return;
            request["status"] = "failed";
            request["error"] = error.Message;
            try { await SaveAsync(requestPath, request).ConfigureAwait(false); }
            catch (Exception saveError) when (saveError is IOException or UnauthorizedAccessException) { }
        }
    }

    private static async Task SaveAsync(string path, JsonObject request)
    {
        string temporary = path + ".tmp";
        await File.WriteAllTextAsync(temporary, request.ToJsonString()).ConfigureAwait(false);
        File.Move(temporary, path, overwrite: true);
    }
}
