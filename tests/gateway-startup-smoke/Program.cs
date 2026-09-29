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
    Console.WriteLine("PASS: cold/concurrent startup, reuse, restart, cancellation, remote endpoint.");
}
finally
{
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
