namespace KYNXA_Desktop.Services;

/// <summary>Creates independently owned gateway clients over a shared loopback connection pool.</summary>
/// <remarks>网关客户端分别持有配置与取消范围；仅回环连接池共用，不保存 Cookie。</remarks>
internal static class GatewayHttpClient
{
    private static readonly SocketsHttpHandler LoopbackTransport = new()
    {
        UseProxy = false,
        UseCookies = false,
        PooledConnectionIdleTimeout = TimeSpan.FromMinutes(5)
    };

    public static HttpClient Create(Uri address, TimeSpan timeout)
    {
        // Uri classifies literal loopback addresses and localhost; similar remote names keep system proxy handling.
        // 仅字面回环地址与 localhost 直连；相似的远端域名仍使用系统代理。
        bool isLoopback = address.IsLoopback;
        return new HttpClient(isLoopback ? LoopbackTransport : new SocketsHttpHandler(), disposeHandler: !isLoopback)
        {
            BaseAddress = address,
            Timeout = timeout
        };
    }
}
