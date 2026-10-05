namespace KYNXA_Desktop.Services;

internal static class ModelGatewayService
{
    public static Uri Address => new("http://127.0.0.1:1");
    public static Task EnsureReadyAsync(CancellationToken cancellationToken = default) => Task.CompletedTask;
}
