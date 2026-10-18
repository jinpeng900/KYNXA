using System.ComponentModel;
using System.Globalization;
using System.Text.Json;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Views;

public sealed partial class LocalModelsPage
{
    /// <summary>Rows retain identity while download progress changes; virtualized containers are not rebuilt.</summary>
    /// <remarks>下载进度变化时保留行身份，不重建虚拟化容器或用户正在操作的控件。</remarks>
    private sealed class ModelRow(LocalModelInfo model) : INotifyPropertyChanged
    {
        private bool _globalEnabled, _busy;
        public LocalModelInfo Model { get; private set; } = model;
        public string Id => Model.Id;
        public string Title => string.IsNullOrWhiteSpace(Model.Name) ? Model.Id : Model.Name;
        public string Metadata => string.Join(" · ", new[] { Model.Family, ParameterText(Model.Parameters), Model.Quantization,
            FormatBytes(Model.DownloadBytes) }.Where(value => !string.IsNullOrWhiteSpace(value)));
        public string Requirements => string.Format(UiText.Get("建议内存 {0} GB · 显存 {1} GB"),
            Number(Model.RecommendedRamGb), Number(Model.RecommendedVramGb));
        public string Features => string.Join(" · ", (Model.Capabilities ?? []).Select(capability => UiText.Get(capability switch
        { "text" => "文本", "vision" => "图像理解", "tools" => "工具调用", "code" => "代码", _ => capability })));
        public string Category => UiText.Get(Model.Category switch
        { "general" => "通用对话", "reasoning" => "推理", "code" => "代码", "vision" => "视觉", _ => Model.Category ?? "" });
        public string State => Model.Installed && Model.Runtime is { State: "preparing" or "preparing-runtime" or "loading" or "ready" or "sleeping" or "failed" } runtime
            ? UiText.Get(runtime.State switch { "preparing" or "preparing-runtime" => "准备中", "loading" => "加载中", "ready" => runtime.ModelIdentityVerified ? "已就绪" : "模型待核验",
                "sleeping" => "休眠待唤醒", _ => "运行失败" })
            : UiText.Get(Model.Status switch
        {
            "queued" or "pending" => "等待下载", "downloading" or "resuming" => "下载中", "paused" => "已暂停",
            "verifying" => "校验中", "installing" or "copying" or "importing" => "安装中", "moving" => "迁移中",
            "loading" => "加载中", "missing" => "文件缺失", "failed" or "error" => "失败",
            "cancelled" => "已取消", _ => Model.Installed ? Model.Enabled ? "已启用" : "已安装" : "可安装"
        });
        public string ProgressText => Model.DownloadedBytes > 0 && !Model.Installed
            ? FormatBytes(Model.DownloadedBytes) + " / " + FormatBytes(Model.DownloadBytes) : "";
        public double Progress => Model.DownloadBytes > 0 ? Math.Clamp(100d * Model.DownloadedBytes / Model.DownloadBytes, 0, 100) : 0;
        public string EnableLabel => UiText.Get(Model.Enabled ? "停用" : "启用");
        public string InstallLabel => UiText.Get(Model.Status is "failed" or "error" or "cancelled" ? "重新安装" : "安装");
        public string PauseLabel => UiText.Get("暂停下载");
        public string ResumeLabel => UiText.Get("继续下载");
        public string CancelLabel => UiText.Get("取消下载");
        public string DeleteLabel => UiText.Get("删除");
        public string RecommendedLabel => Model.Recommended ? UiText.Get("适合本机") : "";
        public string PathText => Model.Path ?? "";
        public Visibility PathVisibility => string.IsNullOrWhiteSpace(Model.Path) ? Visibility.Collapsed : Visibility.Visible;
        // Catalog capabilities describe the model family; runtime readiness needs an observed identity and template.
        // 目录能力描述模型类别；实际就绪须有模型身份与工具模板观测，连接成功不能冒充可调用工具。
        public string RuntimeDescription => Model.Installed && Model.Runtime is { } runtime
            ? string.Join(" · ", new[]
            {
                UiText.Get(runtime.ModelIdentityVerified ? "模型已核验" : "模型待核验"),
                UiText.Get((runtime.ModelIdentityVerified, runtime.SupportsTools, runtime.ToolCallingMode) switch
                {
                    (true, true, "adapted") => "适配工具可用",
                    (true, true, "native") => "原生工具可用",
                    (true, false, _) => "工具调用不可用",
                    _ => "工具能力待检测"
                })
            }) : "";
        public string Description => string.Join(" · ", new[] { Category, Features, RuntimeDescription }.Where(value => !string.IsNullOrWhiteSpace(value)));
        public string ErrorText => (Model.Error.ValueKind switch
        {
            JsonValueKind.String => Model.Error.GetString() ?? "",
            JsonValueKind.Object when Model.Error.TryGetProperty("message", out var message) => message.GetString() ?? "",
            _ => ""
        }) is { Length: > 0 } error ? error : Model.Runtime?.Code switch
        {
            "LOCAL_MODEL_IDENTITY_MISMATCH" => UiText.Get("服务加载的模型与所选文件不一致。"),
            "LOCAL_MODEL_IDENTITY_UNCONFIRMED" => UiText.Get("尚未确认当前服务加载的模型文件。"),
            _ => ""
        };
        public bool ActionsEnabled => !_busy;
        public bool EnableAllowed => !_busy && _globalEnabled;
        public Visibility InstalledVisibility => Model.Installed ? Visibility.Visible : Visibility.Collapsed;
        public Visibility InstallVisibility => Model.Family != "Imported" && !Model.Installed && !IsTransfer(Model) && Model.Status != "paused"
            ? Visibility.Visible : Visibility.Collapsed;
        public Visibility DeleteVisibility => string.IsNullOrEmpty(Model.Path) ? Visibility.Collapsed : Visibility.Visible;
        public Visibility PauseVisibility => Model.Status is "downloading" or "queued" or "pending" or "resuming"
            ? Visibility.Visible : Visibility.Collapsed;
        public Visibility ResumeVisibility => Model.Status == "paused" ? Visibility.Visible : Visibility.Collapsed;
        public Visibility CancelVisibility => !Model.Installed && (IsTransfer(Model) || Model.Status == "paused")
            ? Visibility.Visible : Visibility.Collapsed;
        public Visibility ProgressVisibility => !Model.Installed && (IsTransfer(Model) || Model.Status == "paused")
            ? Visibility.Visible : Visibility.Collapsed;
        public Visibility ErrorVisibility => string.IsNullOrEmpty(ErrorText) ? Visibility.Collapsed : Visibility.Visible;
        public event PropertyChangedEventHandler? PropertyChanged;

        public void Apply(LocalModelInfo next, bool globalEnabled, bool busy)
        {
            Model = next;
            _globalEnabled = globalEnabled;
            _busy = busy;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(null));
        }

        private static string Number(double? value) => value is > 0 ? value.Value.ToString("0.#", CultureInfo.CurrentCulture) : "—";
        private static string ParameterText(JsonElement value) => value.ValueKind switch
        { JsonValueKind.String => value.GetString() ?? "", JsonValueKind.Number => value.GetRawText(), _ => "" };
    }

    private static bool IsTransfer(LocalModelInfo model) => model.Status is "queued" or "pending" or "downloading" or "resuming"
        or "verifying" or "installing" or "copying" or "importing" or "moving";

    private static string FormatBytes(long bytes) => bytes <= 0 ? "—" : bytes >= 1024L * 1024 * 1024
        ? (bytes / (1024d * 1024 * 1024)).ToString("0.##", CultureInfo.CurrentCulture) + " GiB"
        : (bytes / (1024d * 1024)).ToString("0.#", CultureInfo.CurrentCulture) + " MiB";
}
