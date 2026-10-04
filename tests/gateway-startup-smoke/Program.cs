using System.Diagnostics;
using System.Net;
using System.Net.Sockets;
using KYNXA_Desktop.Services;

var log = Path.Combine(Path.GetTempPath(), $"kynxa-gateway-test-{Guid.NewGuid():N}.log");
var listener = new TcpListener(IPAddress.Loopback, 0);
listener.Start();
var port = ((IPEndPoint)listener.LocalEndpoint).Port;
listener.Stop();
Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", $"http://127.0.0.1:{port}");
Environment.SetEnvironmentVariable("KYNXA_STARTUP_TEST_LOG", log);
try
{
    foreach (var (legacyAgentVersion, legacyContextVersion, legacyToolVersion, legacyOfficialVersion, legacyHostVersion, legacyBrowserVersion) in new[]
    { (1, 3, 3, 2, 3, 2), (4, 3, 3, 2, 3, 2), (5, 1, 3, 2, 3, 2), (5, 2, 3, 2, 3, 2), (5, 3, 2, 2, 3, 2),
        (5, 3, 3, 0, 3, 2), (5, 3, 3, 1, 3, 2), (5, 3, 3, 2, 0, 2), (5, 3, 3, 2, 1, 2), (5, 3, 3, 2, 2, 2), (5, 3, 3, 2, 3, 0), (5, 3, 3, 2, 3, 1) })
    using (var legacyReservation = new TcpListener(IPAddress.Loopback, 0))
    {
        legacyReservation.Start();
        int legacyPort = ((IPEndPoint)legacyReservation.LocalEndpoint).Port;
        legacyReservation.Stop();
        using var legacy = new HttpListener();
        legacy.Prefixes.Add($"http://127.0.0.1:{legacyPort}/"); legacy.Start();
        var respond = Task.Run(async () =>
        {
            var request = await legacy.GetContextAsync();
            byte[] bytes = System.Text.Json.JsonSerializer.SerializeToUtf8Bytes(new
            {
                status = "ok", service = "kynxa-model-gateway", conversationProtocol = 1,
                dataLayoutVersion = 1, memoryProtocol = 1, contextProtocol = legacyContextVersion,
                agentProtocol = legacyAgentVersion, officialToolsProtocol = legacyOfficialVersion, hostTerminalProtocol = legacyHostVersion, browserAutomationProtocol = legacyBrowserVersion, extensionStorageProtocol = 1, toolStreamProtocol = legacyToolVersion
            });
            request.Response.ContentType = "application/json";
            await request.Response.OutputStream.WriteAsync(bytes); request.Response.Close();
        });
        Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", $"http://127.0.0.1:{legacyPort}");
        try { await ModelGatewayService.EnsureReadyAsync(); throw new Exception("Legacy agent gateway was reused."); }
        catch (InvalidOperationException error) when (error.Message.Contains("旧网关")) { }
        finally { Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", $"http://127.0.0.1:{port}"); }
        await respond;
    }
    string initializedRoot = Path.Combine(Path.GetTempPath(), "kynxa-storage-helper-" + Guid.NewGuid().ToString("N"));
    Directory.CreateDirectory(initializedRoot);
    await ModelGatewayService.InitializeStorageAsync(initializedRoot);
    if (File.ReadAllText(Path.Combine(initializedRoot, "initialized.txt")) != initializedRoot)
        throw new Exception("Storage helper did not receive the explicit target.");
    Environment.SetEnvironmentVariable("KYNXA_INITIALIZER_TEST_MODE", "failure");
    try { await ModelGatewayService.InitializeStorageAsync(initializedRoot); throw new Exception("Failed initializer accepted."); }
    catch (InvalidOperationException error) when (error.Message.Contains("fixture initialization failure")) { }
    Environment.SetEnvironmentVariable("KYNXA_INITIALIZER_TEST_MODE", "wait");
    string cancelledRoot = Path.Combine(Path.GetTempPath(), "kynxa-storage-helper-" + Guid.NewGuid().ToString("N"));
    Directory.CreateDirectory(cancelledRoot);
    using (var cancelInitializer = new CancellationTokenSource())
    {
        Task initialization = ModelGatewayService.InitializeStorageAsync(cancelledRoot, cancelInitializer.Token);
        var waitForStart = Stopwatch.StartNew();
        while (!File.Exists(Path.Combine(cancelledRoot, "initializer.pid")) && waitForStart.Elapsed < TimeSpan.FromSeconds(5))
            await Task.Delay(25);
        if (!File.Exists(Path.Combine(cancelledRoot, "initializer.pid"))) throw new Exception("Initializer did not start.");
        int pid = int.Parse(File.ReadAllText(Path.Combine(cancelledRoot, "initializer.pid")));
        cancelInitializer.Cancel();
        try { await initialization; throw new Exception("Initializer cancellation ignored."); }
        catch (OperationCanceledException) { }
        try
        {
            using var child = Process.GetProcessById(pid);
            if (!child.HasExited) throw new Exception("Cancelled initializer kept running.");
        }
        catch (ArgumentException) { }
    }
    Environment.SetEnvironmentVariable("KYNXA_INITIALIZER_TEST_MODE", null);
    await Task.WhenAll(Enumerable.Range(0, 8).Select(_ => ModelGatewayService.EnsureReadyAsync()));
    var starts = File.ReadAllLines(log);
    if (starts.Length != 1) throw new Exception("Concurrent startup spawned duplicate services.");
    await ModelGatewayService.EnsureReadyAsync();
    if (File.ReadAllLines(log).Length != 1) throw new Exception("Existing service was not reused.");
    using (var child = Process.GetProcessById(int.Parse(starts[0])))
    {
        child.Kill();
        await child.WaitForExitAsync();
    }
    await ModelGatewayService.EnsureReadyAsync();
    if (File.ReadAllLines(log).Length != 2) throw new Exception("Stopped service was not restarted.");
    using var cancelled = new CancellationTokenSource();
    cancelled.Cancel();
    try { await ModelGatewayService.EnsureReadyAsync(cancelled.Token); throw new Exception("Cancellation ignored."); }
    catch (OperationCanceledException) { }
    Environment.SetEnvironmentVariable("KYNXA_MODEL_API_URL", "https://example.invalid");
    await ModelGatewayService.EnsureReadyAsync();
    if (File.ReadAllLines(log).Length != 2) throw new Exception("Remote endpoint started a local service.");
    Console.WriteLine("PASS: storage initializer success/failure, pipe draining, cancelled child cleanup; gateway cold/concurrent startup, reuse, restart, cancellation, remote endpoint.");
}
finally
{
    Environment.SetEnvironmentVariable("KYNXA_INITIALIZER_TEST_MODE", null);
    if (File.Exists(log))
    {
        foreach (var pid in File.ReadAllLines(log))
        {
            try { using var child = Process.GetProcessById(int.Parse(pid)); child.Kill(); await child.WaitForExitAsync(); }
            catch (ArgumentException) { }
        }
        File.Delete(log);
    }
}
