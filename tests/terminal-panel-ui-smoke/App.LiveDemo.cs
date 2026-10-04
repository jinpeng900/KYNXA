using System.Diagnostics;
using System.Text.Json;
using KYNXA.Contracts;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using KYNXA_Desktop.ViewModels;
using MemoryUiSmoke;

namespace TerminalPanelUiSmoke;

public partial class App
{
    private async Task RunDemoAsync()
    {
        Process? helper = null;
        try
        {
            string? toolHost = FindToolHost();
            if (toolHost is null) throw new FileNotFoundException("Build the repository's native ToolHost before running the isolated demo.");
            string script = "Write-Output 'KYNXA TERMINAL READY'; 1..5 | ForEach-Object { Write-Output ('Progress ' + $_ + '/5'); Start-Sleep -Milliseconds 250 }; Write-Output '本机终端实时输出完成'";
            var running = new ToolActivity("demo-native", "terminal.host.run", JsonSerializer.SerializeToElement(new { shell = "powershell", script, cwd = _directory }), "running", "", WorkspaceRoot: _directory);
            var row = new ConversationMessageViewModel(_chatA, new ChatMessageState { Id = _request, Role = "assistant", Status = "streaming", ToolActivities = [running] });
            _panel.ShowConversation(_chatA, [row]); _panel.SetPreviewEnabled(true); _panel.Observe(_chatA, _request, running);
            var info = new ProcessStartInfo(toolHost) { UseShellExecute = false, CreateNoWindow = true,
                RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true };
            helper = Process.Start(info) ?? throw new IOException("Cannot start the isolated native helper.");
            var owned = helper;
            _window.Closed += (_, _) => { try { if (!owned.HasExited) owned.Kill(entireProcessTree: true); } catch (InvalidOperationException) { } };
            _panel.StopRequested += (_, identity) => { if (identity == new TerminalRunIdentity(_chatA, _request, "demo-native")) { try { owned.StandardInput.WriteLine("cancel"); owned.StandardInput.Flush(); } catch (IOException) { } } };
            Task<string> stderr = helper.StandardError.ReadToEndAsync();
            await helper.StandardInput.WriteLineAsync(JsonSerializer.Serialize(new { operation = "host_terminal", shell = "powershell", script, cwd = _directory, timeoutMs = 10000 }));
            await helper.StandardInput.FlushAsync();
            JsonElement? receipt = null;
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(20));
            while (await helper.StandardOutput.ReadLineAsync(deadline.Token) is string line)
            {
                using var document = JsonDocument.Parse(line);
                var value = document.RootElement;
                if (value.TryGetProperty("event", out var nativeEvent))
                {
                    if (nativeEvent.GetString() == "host_terminal_output")
                    {
                        string text = value.TryGetProperty("delta", out var delta) ? delta.GetString() ?? "" : value.GetProperty("text").GetString() ?? "";
                        _panel.Append(_chatA, _request, new("demo-native", value.GetProperty("sequence").GetInt32(), value.GetProperty("stream").GetString()!, text,
                            value.TryGetProperty("replace", out var replace) && replace.ValueKind == JsonValueKind.True));
                    }
                }
                else receipt = value.Clone();
            }
            await helper.WaitForExitAsync(deadline.Token); await stderr;
            if (receipt is not JsonElement final || !final.TryGetProperty("completed", out var completed) || completed.ValueKind != JsonValueKind.True ||
                !final.TryGetProperty("exitCode", out var exit) || exit.GetInt32() != 0) throw new InvalidDataException("The real command did not return a completed successful receipt.");
            var result = running with { Status = "completed", Result = final.GetRawText() };
            row.Message.ToolActivities[0] = result; row.Message.Status = "completed"; row.Refresh(); _panel.Observe(_chatA, _request, result);
            await WaitAsync(() => Output.Text.Contains("KYNXA TERMINAL READY") && Output.Text.Contains("5/5") &&
                Status.Text.StartsWith("执行完成", StringComparison.Ordinal) && Status.Text.Contains("退出码 0"),
                "actual native output and completion receipt rendered in production sidebar");
            await NativeWindowCapture.CaptureAsync(_window, Path.Combine(_directory, "terminal-live-demo.png"));
            File.WriteAllText(Path.Combine(_directory, "result.txt"), "PASS: real isolated PowerShell command streamed to production terminal panel.\n");
            await Task.Delay(30000);
        }
        catch (Exception error) { File.WriteAllText(Path.Combine(_directory, "result.txt"), "FAIL live demo: " + error); }
        finally
        {
            if (helper is not null) { try { if (!helper.HasExited) helper.Kill(entireProcessTree: true); } catch (InvalidOperationException) { } helper.Dispose(); }
            _panel.Dispose(); _window.Close(); Exit();
        }
    }

    private static string? FindToolHost()
    {
        var arguments = Environment.GetCommandLineArgs();
        int explicitIndex = Array.IndexOf(arguments, "--tool-host");
        if (explicitIndex >= 0 && explicitIndex + 1 < arguments.Length && Path.IsPathFullyQualified(arguments[explicitIndex + 1]) && File.Exists(arguments[explicitIndex + 1]))
            return arguments[explicitIndex + 1];
        for (var root = new DirectoryInfo(AppContext.BaseDirectory); root is not null; root = root.Parent)
        {
            string directory = Path.Combine(root.FullName, "apps", "tool-host", "bin");
            if (!Directory.Exists(directory)) continue;
            return Directory.EnumerateFiles(directory, "KYNXA.ToolHost.exe", SearchOption.AllDirectories).OrderByDescending(File.GetLastWriteTimeUtc).FirstOrDefault();
        }
        return null;
    }
}
