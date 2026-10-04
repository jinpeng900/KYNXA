namespace KYNXA_Desktop.Services;

internal static class ModelGatewayService
{
    public static Uri Address => new("http://127.0.0.1:9");
    public static Task EnsureReadyAsync(CancellationToken cancellationToken) =>
        throw new InvalidOperationException("Only the injected synthetic work-pane API is permitted.");
}
