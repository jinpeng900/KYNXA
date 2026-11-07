// Only desktop startup and localization are replaced; protocols, recovery and formatting use production code.
// 仅替换桌面启动和本地化入口；协议、恢复与排版直接使用生产源码。
namespace KYNXA_Desktop.Services;

internal static class UiText { public static string Get(string text) => text; }
internal static class GatewayHttpClient
{
    public static HttpClient Create(Uri address, TimeSpan timeout) => new() { BaseAddress = address, Timeout = timeout };
}
internal static class ModelGatewayService
{
    public static Uri Address => new("http://127.0.0.1:1");
    public static Task EnsureReadyAsync(CancellationToken cancellationToken) => Task.CompletedTask;
}
