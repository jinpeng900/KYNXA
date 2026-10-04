namespace KYNXA_Desktop.Services;

// Linked AgentApiClient's production constructor must never start the user's real gateway in this isolated fixture.
// 此隔离夹具链接的 AgentApiClient 生产构造函数不得启动用户真实网关。
internal static class ModelGatewayService
{
    public static Uri Address => new("http://127.0.0.1:9");
    public static Task EnsureReadyAsync(CancellationToken cancellationToken) => throw new InvalidOperationException("The smoke uses only its injected fake API.");
}
