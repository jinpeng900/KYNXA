namespace KYNXA_Desktop.Services;

/// <summary>The isolated UI host must inject its memory API and never start a gateway.</summary>
/// <remarks>独立 UI 宿主必须注入记忆 API，不得启动真实网关或读取正式数据。</remarks>
public static class ModelGatewayService
{
    public static Uri Address => new("http://memory.test.invalid");
    public static Task EnsureReadyAsync(CancellationToken cancellationToken = default) =>
        throw new InvalidOperationException("Native memory smoke must inject its synthetic memory API.");
}
