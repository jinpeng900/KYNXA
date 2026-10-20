using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace KYNXA_Desktop.Views;

public sealed partial class RetrievalSettingsWindow
{
    /// <summary>
    /// Imports only the entered links into this fixed scope; partial outcomes remain visible.
    /// 只将输入的明确链接导入当前固定范围；部分成功及未确认结果保留提示。
    /// </summary>
    private async Task ImportLinksAsync()
    {
        await RunOperationAsync(async () =>
        {
            var input = new TextBox { AcceptsReturn = true, TextWrapping = TextWrapping.Wrap, MinHeight = 120,
                MaxHeight = 280, MaxLength = 32768, PlaceholderText = "https://example.com/document" };
            var error = new TextBlock { TextWrapping = TextWrapping.Wrap, FontSize = 12 };
            var body = new StackPanel { Spacing = 10 };
            body.Children.Add(Label("每行一个 HTTP 或 HTTPS 链接。仅保存这些页面或文档，不抓取整个网站。", 12));
            body.Children.Add(input); body.Children.Add(error);
            string[] links = [];
            var dialog = new ContentDialog { XamlRoot = DialogRoot,
                Title = UiText.Get("添加链接资料"), Content = body, PrimaryButtonText = UiText.Get("导入"),
                CloseButtonText = UiText.Get("取消"), DefaultButton = ContentDialogButton.Primary };
            dialog.PrimaryButtonClick += (_, args) =>
            {
                links = input.Text.Split(['\r', '\n'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)
                    .Distinct(StringComparer.Ordinal).ToArray();
                string? invalid = links.Length is < 1 or > 64 ? UiText.Get("请输入 1～64 个链接。") : null;
                if (invalid is null)
                    for (int index = 0; index < links.Length; index++)
                        if (!Uri.TryCreate(links[index], UriKind.Absolute, out var uri) || uri.Scheme is not ("http" or "https")
                            || !string.IsNullOrEmpty(uri.UserInfo))
                        { invalid = string.Format(UiText.Get("第 {0} 个链接不是有效的 HTTP(S) 地址。"), index + 1); break; }
                if (invalid is not null) { error.Text = invalid; args.Cancel = true; }
            };
            if (await DialogPresentation.ShowAsync(dialog) != ContentDialogResult.Primary || _closed) return;
            var failures = new List<string>();
            int importedCount = 0;
            string? latestJobId = null;
            // Individual bounded requests preserve progress when another URL fails or times out.
            // 逐条有界请求让已导入结果立即生效；单条链接失败或超时不撤销之前资料。
            for (int index = 0; index < links.Length && !_closed; index++)
            {
                _indexStatus.Text = string.Format(UiText.Get("正在导入链接 · {0}/{1}"), index + 1, links.Length);
                try
                {
                    var result = await _api.ImportSourceAsync(new(_projectId is null ? "user" : "project",
                        ProjectId: _projectId, Url: links[index]), _lifetime.Token);
                    importedCount += result.ImportedCount ?? 1;
                    latestJobId = result.JobId ?? latestJobId;
                    if (result.SourceCoverage is { Complete: false } coverage)
                        failures.Add($"{links[index]}\n{CoverageFailureText(coverage)}");
                }
                catch (OperationCanceledException) when (_closed) { return; }
                catch (Exception failure)
                {
                    string message = failure is OperationCanceledException
                        ? UiText.Get("导入回执超时，结果未确认；请刷新资料列表核对，不要直接重复提交。") : failure.Message;
                    failures.Add($"{links[index]}\n{message}");
                }
            }
            if (_closed) return;
            RenderSources((await _api.GetSourcesAsync(_projectId, _lifetime.Token)).Sources);
            if (latestJobId is not null) ShowJob(await _api.GetIndexJobAsync(latestJobId, _lifetime.Token));
            _notice.Message = string.Format(UiText.Get("已导入 {0} 份资料。"), importedCount)
                + (failures.Count == 0 ? "" : "\n" + string.Join("\n\n", failures));
            _notice.Severity = failures.Count == 0 ? InfoBarSeverity.Success : InfoBarSeverity.Warning;
            _notice.IsOpen = true;
        });
    }

    private static string CoverageFailureText(RetrievalSourceCoverage coverage)
    {
        if (coverage.Failures is not { ValueKind: JsonValueKind.Array } failures) return UiText.Get("资料仅部分解析，请查看准备状态。");
        return string.Join("\n", failures.EnumerateArray().Take(8).Select(failure =>
        {
            string location = failure.TryGetProperty("relativePath", out var path) && path.ValueKind == JsonValueKind.String ? path.GetString() ?? "" : "";
            string code = failure.TryGetProperty("errorCode", out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() ?? "" : "";
            return $"{location}: {code}";
        })) + (coverage.Failed > 8 ? string.Format(UiText.Get("\n另有 {0} 项失败，完整记录保留在资料准备回执。"), coverage.Failed - 8) : "");
    }
}
