namespace KYNXA_Desktop.Services;

// The linked transport is exercised through an injected HTTP handler. Never start a real gateway or access Data.
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
