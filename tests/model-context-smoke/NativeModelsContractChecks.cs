using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using KYNXA_Desktop.Services;

internal static class NativeModelsContractChecks
{
    // The standalone smoke has no bundled gateway, so a failed health check cannot launch a process.
    // 独立短测不带网关可执行文件，健康检查失败不能启动进程；仅读取列表并恢复临时环境变量。
    public static async Task RunLiveAsync(string loopbackUrl)
    {
        Check(Uri.TryCreate(loopbackUrl, UriKind.Absolute, out var address) && address.Scheme == "http"
            && address.Host is "127.0.0.1" or "localhost" && address.UserInfo.Length == 0
            && address.AbsolutePath == "/" && address.Query.Length == 0 && address.Fragment.Length == 0,
            "Use a credential-free loopback HTTP base URL.");
        Check(!File.Exists(Path.Combine(AppContext.BaseDirectory, "runtime", "agent", "kynxa-gateway.exe")),
            "Run the standalone smoke output without a bundled gateway to guarantee read-only checks.");
        string? previousAddress = Environment.GetEnvironmentVariable("KYNXA_MODEL_API_URL");
        Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", address!.AbsoluteUri);
        try
        {
            using var cancellation = new CancellationTokenSource(TimeSpan.FromSeconds(15));
            using var client = new ModelApiClient();
            ModelProvider[] providers = await client.ListAsync(cancellation.Token);
            Check(providers is { Length: > 0 }, "The native model list is empty.");
            foreach (var provider in providers)
            {
                ModelApiClient.ValidateContextWindowTokens(provider.ContextWindowTokens);
                ModelApiClient.ValidateMaxOutputTokens(provider.MaxOutputTokens);
                Check(!string.IsNullOrWhiteSpace(provider.ProviderId) && provider.Models is { Length: > 0 }
                    && provider.Models.All(model => !string.IsNullOrWhiteSpace(model)), "A provider has invalid model identities.");
                Console.WriteLine(JsonSerializer.Serialize(new { providerId = provider.ProviderId, modelIds = provider.Models,
                    contextWindowTokens = provider.ContextWindowTokens, maxOutputTokens = provider.MaxOutputTokens }));
            }
            Console.WriteLine($"PASS: native health and desktop model-list restoration; providers={providers.Length}.");
        }
        finally { Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", previousAddress); }
    }

    // A Rust-generated public response passes through the actual desktop API client over owned HTTP.
    // Rust 生成的公共响应经自有 HTTP 进入真实桌面 API 客户端；不读取真实模型、凭据或聊天配置。
    public static async Task RunAsync(string fixturePath)
    {
        byte[] publicResponse = await File.ReadAllBytesAsync(Path.GetFullPath(fixturePath));
        using (var document = JsonDocument.Parse(publicResponse))
        {
            foreach (var provider in document.RootElement.GetProperty("providers").EnumerateArray())
            {
                Check(!provider.TryGetProperty("apiKey", out _), "The public fixture exposed a private key field.");
                Check(provider.GetProperty("contextWindowMode").GetString() == "automatic",
                    "Legacy transport defaults changed actual capacity mode.");
            }
        }
        using var reservation = new TcpListener(IPAddress.Loopback, 0);
        reservation.Start();
        int port = ((IPEndPoint)reservation.LocalEndpoint).Port;
        reservation.Stop();
        using var listener = new HttpListener();
        listener.Prefixes.Add($"http://127.0.0.1:{port}/");
        listener.Start();
        var server = Task.Run(async () =>
        {
            for (int index = 0; index < 2; index++)
            {
                var context = await listener.GetContextAsync();
                string path = context.Request.Url!.AbsolutePath;
                Check(context.Request.HttpMethod == "GET", "The fixture unexpectedly tried to save or probe a model.");
                byte[] bytes = path switch
                {
                    "/health" => Encoding.UTF8.GetBytes("{\"service\":\"kynxa-model-gateway\",\"status\":\"ok\",\"runtime\":\"rust\",\"conversationProtocol\":1,\"dataLayoutVersion\":1,\"memoryProtocol\":1,\"contextProtocol\":3,\"agentProtocol\":5,\"officialToolsProtocol\":2,\"hostTerminalProtocol\":3,\"browserAutomationProtocol\":2,\"extensionStorageProtocol\":1,\"toolStreamProtocol\":3,\"retrievalProtocol\":1}"),
                    "/api/models" => publicResponse,
                    _ => throw new InvalidOperationException("The fixture requested an unexpected API.")
                };
                context.Response.ContentType = "application/json";
                context.Response.ContentLength64 = bytes.Length;
                await context.Response.OutputStream.WriteAsync(bytes);
                context.Response.Close();
            }
        });
        string? previousAddress = Environment.GetEnvironmentVariable("KYNXA_MODEL_API_URL");
        Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", $"http://127.0.0.1:{port}");
        try
        {
            using var client = new ModelApiClient();
            ModelProvider[] providers = await client.ListAsync().WaitAsync(TimeSpan.FromSeconds(15));
            Check(providers.Length == 3, "A malformed provider prevented the complete list from loading.");
            var missing = providers.Single(provider => provider.ProviderId == "fixture-missing");
            var nullWindow = providers.Single(provider => provider.ProviderId == "fixture-null");
            var explicitWindow = providers.Single(provider => provider.ProviderId == "fixture-explicit");
            Check(missing.ContextWindowTokens == 32768 && nullWindow.ContextWindowTokens == 32768,
                "Missing or null legacy context limits broke the desktop integer contract.");
            Check(missing.MaxOutputTokens == 262144 && nullWindow.MaxOutputTokens == 262144,
                "Legacy context compatibility changed independent output defaults.");
            Check(explicitWindow.ContextWindowTokens == 1_000_000 && explicitWindow.MaxOutputTokens == 2048,
                "A valid stored context or small output setting was changed.");
            Check(missing.HasApiKey && missing.Models.SequenceEqual(["fixture-qwen"])
                && explicitWindow.Models.SequenceEqual(["fixture-qwen", "fixture-qwen-two"]),
                "Provider identity, model selection or the public key-presence flag was lost.");
            await server.WaitAsync(TimeSpan.FromSeconds(15));
            Console.WriteLine("PASS: actual native public model response loads all three providers through the desktop client; missing/null defaults, explicit limits and model selection preserved; no private key field.");
        }
        finally
        {
            Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", previousAddress);
            listener.Stop();
        }
    }

    private static void Check(bool condition, string message)
    {
        if (!condition) throw new InvalidOperationException(message);
    }
}
