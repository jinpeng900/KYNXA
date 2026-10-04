namespace KYNXA_Desktop.Services;

// Linked AgentApiClient's production constructor must never start the user's real gateway in this isolated fixture.
internal static class ModelGatewayService
{
    public static Uri Address => new("http://127.0.0.1:9");
    public static Task EnsureReadyAsync(CancellationToken cancellationToken) => throw new InvalidOperationException("The smoke uses only its injected fake API.");
}
