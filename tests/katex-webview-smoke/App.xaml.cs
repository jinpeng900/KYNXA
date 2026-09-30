using System.Text.Json;
using System.Diagnostics;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.Web.WebView2.Core;
using SkiaSharp;
using KYNXA_Desktop.Services;

namespace KatexWebViewSmoke;

public partial class App : Application
{
    private Window? _window;
    private readonly string _result = Path.Combine(Path.GetTempPath(), "kynxa-katex-webview-probe.txt");
    private WebView2? _view;

    public App()
    {
        InitializeComponent();
        UnhandledException += (_, e) => File.WriteAllText(_result, "FAIL: " + e.Exception);
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        bool benchmark = Environment.GetCommandLineArgs().Contains("--benchmark");
        if (Environment.GetCommandLineArgs().Contains("--service") || benchmark)
        {
            var label = new TextBlock { Text = "KaTeX production service capture probe", Margin = new Thickness(12) };
            var host = new Grid { Background = new SolidColorBrush(Colors.White) };
            host.Children.Add(label);
            _window = new Window { Title = "KYNXA KaTeX service probe", Content = host };
            _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(800, 450));
            label.Loaded += async (_, _) => { if (benchmark) await RunBenchmarkAsync(label); else await RunServiceAsync(label); };
            _window.Activate();
            return;
        }
        _view = new WebView2 { Width = 768, Height = 600, IsHitTestVisible = false, IsTabStop = false, Opacity = 0 };
        var root = new Grid { Background = new SolidColorBrush(Colors.White) };
        root.Children.Add(_view);
        root.Children.Add(new TextBlock { Text = "Local WebView2 formula capture probe", Margin = new Thickness(12) });
        _window = new Window { Title = "KYNXA WebView2 formula probe", Content = root };
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(820, 500));
        _view.Loaded += async (_, _) => await RunAsync();
        _window.Activate();
    }

    private async Task RunBenchmarkAsync(FrameworkElement owner)
    {
        string resultPath = Path.Combine(Path.GetTempPath(), "kynxa-katex-benchmark.json");
        var watch = Stopwatch.StartNew();
        long previous = 0, largestGap = 0;
        int ticks = 0;
        var timer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(16) };
        timer.Tick += (_, _) => { long now = watch.ElapsedMilliseconds; largestGap = Math.Max(largestGap, now - previous); previous = now; ticks++; };
        try
        {
            File.WriteAllText(resultPath, "{\"status\":\"running\"}");
            var renderer = KatexFormulaRenderer.ForElement(owner) ?? throw new Exception("Worker missing");
            var formulas = Enumerable.Range(0, 150).Select(index => (index % 5) switch
            {
                0 => $"v_{{{index}}}=\\frac{{s_{{{index}}}}}{{t}}",
                1 => $"\\omega_{{{index}}}=2\\pi f_{{{index}}}",
                2 => $"x_{{{index}}}^2+y^2=z^2",
                3 => $"a_{{{index}}}\\equiv b\\pmod{{{index + 1}}}",
                _ => $"E_{{{index}}}=mc^2"
            }).ToArray();
            timer.Start();
            var cold = await Task.WhenAll(formulas.Select((latex, index) => renderer.RenderAsync(latex, index % 3 == 0))).WaitAsync(TimeSpan.FromSeconds(90));
            long coldMs = watch.ElapsedMilliseconds;
            int success = cold.Count(image => image is not null);
            if (success != formulas.Length) throw new Exception($"Only {success}/{formulas.Length} formulas rendered");
            watch.Restart();
            var warm = await Task.WhenAll(formulas.Select((latex, index) => renderer.RenderAsync(latex, index % 3 == 0)));
            long warmMs = watch.ElapsedMilliseconds;
            bool sameImages = cold.Zip(warm).All(pair => ReferenceEquals(pair.First, pair.Second));
            timer.Stop();
            File.WriteAllText(resultPath, JsonSerializer.Serialize(new { status = "pass", count = success, coldMs, warmMs,
                sameImages, largestUiGapMs = largestGap, ticks, pngBytes = cold.Sum(image => image!.Png.Length) }));
        }
        catch (Exception error) { File.WriteAllText(resultPath, JsonSerializer.Serialize(new { status = "fail", error = error.ToString() })); }
        finally { timer.Stop(); _window?.Close(); }
    }

    private async Task RunServiceAsync(FrameworkElement owner)
    {
        try
        {
            File.WriteAllText(_result, "RUNNING: production service initialization\n");
            var renderer = KatexFormulaRenderer.ForElement(owner) ?? throw new Exception("Worker missing");
            string[] formulas = { @"\omega=2\pi f", @"\tau=RC", @"\epsilon_0=8.85\times10^{-12}", @"\beta=v/c",
                @"1+1\equiv0\pmod2", @"\boxed{\overbrace{a+b}^{n}}", @"\overset{a}{=}\underset{b}{=}",
                @"\begin{aligned}a&=b\\c&=d\end{aligned}", @"\frac{a+b}{c}+\sqrt{x^2+y^2}",
                @"\dfrac{a}{b}", @"\tfrac{a}{b}", @"\bigl(\frac{x}{y}\bigr)", @"\underbrace{a+b}_{n}",
                @"\unknowncommand{x}", @"E=mc^2", @"\begin{cases}x&x>0\\-x&x\le0\end{cases}" };
            var tasks = formulas.Select((latex, index) => renderer.RenderAsync(latex, index % 2 == 0)).ToArray();
            var images = await Task.WhenAll(tasks).WaitAsync(TimeSpan.FromSeconds(45));
            for (int i = 0; i < images.Length; i++)
            {
                var image = images[i] ?? throw new Exception("Formula failed: " + formulas[i]);
                using var bitmap = SKBitmap.Decode(image.Png);
                if (!bitmap.Pixels.Any(pixel => pixel.Alpha > 0 && pixel.Red < 240)) throw new Exception("Blank formula: " + formulas[i]);
                if (image.Baseline <= 0 || image.Baseline > image.Height) throw new Exception("Invalid baseline");
                File.AppendAllText(_result, $"PASS formula {i}: {image.Width:F2}x{image.Height:F2}, baseline={image.Baseline:F2}, {image.Png.Length} bytes\n");
            }
            if (!ReferenceEquals(images[0], await renderer.RenderAsync(formulas[0], true))) throw new Exception("Cache did not return same image");
            if (!ReferenceEquals(images[0], await Task.Run(() => renderer.RenderAsync(formulas[0], true)))) throw new Exception("Dispatcher forwarding failed");
            if (await renderer.RenderAsync(new string('x', 32769), false) is not null) throw new Exception("Input size limit ignored");
            var tallTasks = Enumerable.Range(0, 9).Select(index =>
            {
                string rows = string.Join(@"\\", Enumerable.Range(0, 24).Select(row => $"a_{{{row}}}&=b_{{{index}}}"));
                return renderer.RenderAsync(@"\begin{aligned}" + rows + @"\end{aligned}", true);
            }).ToArray();
            var tallImages = await Task.WhenAll(tallTasks).WaitAsync(TimeSpan.FromSeconds(30));
            if (tallImages.Any(image => image is null || image.Height < 100)) throw new Exception("Overflow batch was not retried individually");
            File.AppendAllText(_result, "PASS: 9 tall formulas overflow the first batch and all complete after individual retries.\n");
            File.WriteAllBytes(Path.Combine(Path.GetTempPath(), "kynxa-katex-service-formula.png"), images[5]!.Png);
            File.AppendAllText(_result, "PASS: production shared worker, 16 formulas in batches, original TeX, glyphs, baselines, cache, dispatcher, overflow retry and size guard.\n");
        }
        catch (Exception error) { File.AppendAllText(_result, "FAIL: " + error); }
    }

    private async Task RunAsync()
    {
        try
        {
            File.WriteAllText(_result, "RUNNING: initialization\n");
            await _view!.EnsureCoreWebView2Async();
            _view.CoreWebView2.Settings.AreDevToolsEnabled = false;
            _view.CoreWebView2.Settings.AreDefaultContextMenusEnabled = false;
            _view.DefaultBackgroundColor = Colors.Transparent;
            var directory = new DirectoryInfo(AppContext.BaseDirectory);
            while (directory is not null && !Directory.Exists(Path.Combine(directory.FullName, "apps/desktop/Resources/Math/KaTeX"))) directory = directory.Parent;
            if (directory is null) throw new DirectoryNotFoundException("KaTeX assets missing");
            _view.CoreWebView2.SetVirtualHostNameToFolderMapping("kynxa-math.local",
                Path.Combine(directory.FullName, "apps/desktop/Resources/Math/KaTeX"), CoreWebView2HostResourceAccessKind.Allow);
            var loaded = new TaskCompletionSource();
            _view.CoreWebView2.NavigationCompleted += (_, e) =>
            {
                if (e.IsSuccess) loaded.TrySetResult();
                else loaded.TrySetException(new Exception(e.WebErrorStatus.ToString()));
            };
            _view.NavigateToString("""
                <!doctype html><html><head>
                <link rel="stylesheet" href="https://kynxa-math.local/katex.min.css">
                <script src="https://kynxa-math.local/katex.min.js"></script><style>
                html,body{margin:0;padding:0;background:transparent;overflow:hidden}
                .row{display:inline-block;margin:8px;font:28px/normal 'Times New Roman';white-space:nowrap;color:#232323}
                .katex{font-size:1em}
                .baseline{display:inline-block;width:0;height:0;padding:0;margin:0;border:0;vertical-align:baseline}
                </style></head><body><div id="rows"></div><script>
                const examples=[String.raw`\omega=2\pi f`,String.raw`1+1\equiv0\pmod2`,String.raw`\boxed{\overbrace{a+b}^{n}}`,String.raw`\overset{a}{=}\underset{b}{=}`,String.raw`\begin{aligned}a&=b\\c&=d\end{aligned}`,String.raw`\frac{a+b}{c}+\sqrt{x^2+y^2}`];
                for(const source of examples){const line=document.createElement('div');const row=document.createElement('span');row.className='row';const formula=document.createElement('span');formula.className='formula';const baseline=document.createElement('span');baseline.className='baseline';row.append(formula,baseline);line.append(row);rows.append(line);katex.render(source,formula,{throwOnError:true,trust:false,displayMode:false});}
                window.ready=false;document.fonts.ready.then(()=>requestAnimationFrame(()=>requestAnimationFrame(()=>window.ready=true)));
                </script></body></html>
                """);
            await loaded.Task.WaitAsync(TimeSpan.FromSeconds(15));
            for (int attempt = 0; attempt < 100 && await _view.ExecuteScriptAsync("window.ready===true") != "true"; attempt++)
                await Task.Delay(50);
            if (await _view.ExecuteScriptAsync("window.ready===true") != "true") throw new TimeoutException("KaTeX/fonts did not become ready");
            foreach (string mode in new[] { "opacity-zero", "visible", "offscreen" })
            {
                _view.Opacity = mode == "opacity-zero" ? 0 : 1;
                _view.RenderTransform = new TranslateTransform { X = mode == "offscreen" ? -5000 : 0 };
                await Task.Delay(250);
                string layout = await _view.ExecuteScriptAsync("Array.from(document.querySelectorAll('.row'),row=>{const r=row.getBoundingClientRect(),b=row.querySelector('.baseline').getBoundingClientRect();return {width:r.width,height:r.height,baseline:b.top-r.top,x:r.x,y:r.y,dpr:devicePixelRatio}})");
                string path = Path.Combine(Path.GetTempPath(), "kynxa-webview-" + mode + ".png");
                using (var stream = File.Create(path))
                    await _view.CoreWebView2.CapturePreviewAsync(CoreWebView2CapturePreviewImageFormat.Png, stream.AsRandomAccessStream());
                using var bitmap = SKBitmap.Decode(path);
                int opaque = bitmap.Pixels.Count(pixel => pixel.Alpha > 0);
                int ink = bitmap.Pixels.Count(pixel => pixel.Alpha > 0 && pixel.Red < 100 && pixel.Green < 100 && pixel.Blue < 100);
                if (ink == 0) throw new Exception(mode + " captured blank image");
                using var metrics = JsonDocument.Parse(layout);
                if (metrics.RootElement.GetArrayLength() != 6) throw new Exception("Not all formulas were rendered");
                foreach (var metric in metrics.RootElement.EnumerateArray())
                {
                    var rect = SKRectI.Create((int)Math.Floor(metric.GetProperty("x").GetDouble()), (int)Math.Floor(metric.GetProperty("y").GetDouble()),
                        (int)Math.Ceiling(metric.GetProperty("width").GetDouble()), (int)Math.Ceiling(metric.GetProperty("height").GetDouble()));
                    using var crop = new SKBitmap();
                    if (!bitmap.ExtractSubset(crop, rect) || !crop.Pixels.Any(pixel => pixel.Alpha > 0 && pixel.Red < 100))
                        throw new Exception("A formula crop is blank");
                    if (metric.GetProperty("baseline").GetDouble() <= 0) throw new Exception("Missing baseline");
                }
                File.AppendAllText(_result, $"PASS {mode}: {bitmap.Width}x{bitmap.Height}, ink={ink}, alpha={opaque}, metrics={layout}, png={path}\n");
            }
            File.AppendAllText(_result, "PASS: all WebView2 hosting modes capture nonempty glyphs.\n");
        }
        catch (Exception error) { File.AppendAllText(_result, "FAIL: " + error); }
    }
}
