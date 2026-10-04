namespace KYNXA_Desktop.Services;

// The linked transport is exercised through an injected HTTP handler. Never start a real gateway or access Data.
// 通过注入 HTTP handler 验证链接的真实传输；不启动真实网关或访问用户 Data。
public static class ModelGatewayService
{
    public static Uri Address => new("http://memory.test.invalid");
    public static Task EnsureReadyAsync(CancellationToken cancellationToken = default) =>
        throw new InvalidOperationException("Smoke tests must inject their HTTP transport.");
}

public static class UiText
{
    public static string Get(string text) => text;
}
