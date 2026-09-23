using KYNXA.Contracts;

var builder = WebApplication.CreateBuilder(args);
builder.WebHost.UseUrls(Environment.GetEnvironmentVariable("KYNXA_MOCK_API_URL") ?? "http://127.0.0.1:5217");
var app = builder.Build();

app.MapGet("/health", () => Results.Ok(new { status = "ok", service = "kynxa-mock-chat" }));
app.MapPost("/api/chat", async (ChatRequest request, CancellationToken cancellationToken) =>
{
    if (request.ConversationId == Guid.Empty || string.IsNullOrWhiteSpace(request.Message))
        return Results.BadRequest(new { error = "conversationId and message are required" });
    if (!ChatPermissionModes.IsSupported(request.PermissionMode))
        return Results.BadRequest(new { error = "permissionMode must be ask, smart, or full" });

    // Permission mode is request metadata only until an execution engine is connected.
    // A short delay makes the waiting state visible during UI development. No model or remote service is called.
    await Task.Delay(650, cancellationToken);
    return Results.Ok(new ChatReply(request.ConversationId, Guid.NewGuid(), "assistant", "你好", DateTimeOffset.UtcNow));
});

app.Run();
