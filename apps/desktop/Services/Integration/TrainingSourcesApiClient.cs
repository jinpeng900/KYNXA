using System.Net.Http.Json;
using System.Text.Json;

namespace KYNXA_Desktop.Services;

// 系统选择仅授权所选来源，网页不能自行调用此入口扩大权限。
internal sealed class TrainingSourcesApiClient : IDisposable
{
    private readonly HttpClient _http = GatewayHttpClient.Create(ModelGatewayService.Address, Timeout.InfiniteTimeSpan);
    public async Task<JsonElement> ImportAsync(Guid conversation, Guid? mount, string action, object payload, CancellationToken cancellation)
    {
        if (action is not ("training.sources.import" or "training.models.register" or "training.environment.configure"))
            throw new InvalidOperationException("训练来源操作无效。");
        await ModelGatewayService.EnsureReadyAsync(cancellation);
        using var response = await _http.PostAsJsonAsync("/agent/workspace/workbench",
            new { conversationId = conversation, mountId = mount, action, payload }, cancellation);
        using var result = await GatewayResponseReader.ReadAsync<JsonDocument>(response,
            "训练来源返回为空。", "训练来源导入失败（HTTP {0}）。", cancellation);
        return result.RootElement.Clone();
    }
    public void Dispose() => _http.Dispose();
}
