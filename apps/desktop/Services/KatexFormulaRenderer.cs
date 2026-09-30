using System.Runtime.CompilerServices;
using System.Text.Json;
using Microsoft.UI;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.Web.WebView2.Core;
using SkiaSharp;
using Windows.Storage.Streams;

namespace KYNXA_Desktop.Services;

/// <summary>One local, noninteractive KaTeX worker per XAML root; native text remains selectable.</summary>
public sealed class KatexFormulaRenderer
{
    private const string HostName = "kynxa-math.local";
    private const long CacheLimit = 16 * 1024 * 1024;
    private static readonly ConditionalWeakTable<XamlRoot, KatexFormulaRenderer> Workers = new();
    private static readonly JsonSerializerOptions JsonOptions = new() { PropertyNameCaseInsensitive = true };
    private readonly XamlRoot _xamlRoot;
    private readonly Grid _host;
    private readonly DispatcherQueue _dispatcher;
    private readonly Dictionary<FormulaKey, FormulaImage> _cache = new();
    private readonly Queue<FormulaKey> _cacheOrder = new();
    private readonly Dictionary<FormulaKey, Request> _pending = new();
    private readonly Queue<Request> _queue = new();
    private readonly Queue<Request> _retryQueue = new();
    private Canvas? _canvas;
    private WebView2? _view;
    private Task? _initialization;
    private TaskCompletionSource<BatchResponse>? _response;
    private string? _responseId;
    private DateTime _retryAfter;
    private long _cacheBytes;
    private bool _running, _disposed;

    private readonly record struct FormulaKey(string Latex, bool Block);
    private sealed record Request(FormulaKey Key, TaskCompletionSource<FormulaImage?> Completion)
    {
        public bool Retried { get; set; }
    }
    private sealed class BatchResponse
    {
        public string? Type { get; set; }
        public string? Id { get; set; }
        public double ViewportWidth { get; set; }
        public double ViewportHeight { get; set; }
        public List<FormulaBounds> Results { get; set; } = new();
    }
    private sealed class FormulaBounds
    {
        public int Index { get; set; }
        public double X { get; set; }
        public double Y { get; set; }
        public double Width { get; set; }
        public double Height { get; set; }
        public double Baseline { get; set; }
        public string? Error { get; set; }
    }

    public static KatexFormulaRenderer? ForElement(FrameworkElement owner)
    {
        if (owner.XamlRoot is not { } root) return null;
        if (Workers.TryGetValue(root, out var existing)) return existing;
        Grid? host = null;
        for (DependencyObject? node = owner; node is not null; node = VisualTreeHelper.GetParent(node))
            if (node is Grid grid) host = grid;
        if (host is null) return null;
        var worker = new KatexFormulaRenderer(root, host);
        Workers.Add(root, worker);
        return worker;
    }

    private KatexFormulaRenderer(XamlRoot root, Grid host)
    {
        _xamlRoot = root;
        _host = host;
        _dispatcher = host.DispatcherQueue;
        host.Unloaded += HostUnloaded;
    }

    public Task<FormulaImage?> RenderAsync(string latex, bool block)
    {
        if (!_dispatcher.HasThreadAccess)
        {
            var completion = new TaskCompletionSource<FormulaImage?>(TaskCreationOptions.RunContinuationsAsynchronously);
            if (!_dispatcher.TryEnqueue(async () =>
            {
                try { completion.TrySetResult(await RenderAsync(latex, block)); }
                catch { completion.TrySetResult(null); }
            })) completion.TrySetResult(null);
            return completion.Task;
        }
        if (_disposed || DateTime.UtcNow < _retryAfter || string.IsNullOrWhiteSpace(latex) || latex.Length > 32768)
            return Task.FromResult<FormulaImage?>(null);
        var key = new FormulaKey(latex, block);
        if (_cache.TryGetValue(key, out var cached)) return Task.FromResult<FormulaImage?>(cached);
        if (_pending.TryGetValue(key, out var pending)) return pending.Completion.Task;
        var request = new Request(key, new(TaskCreationOptions.RunContinuationsAsynchronously));
        _pending.Add(key, request);
        _queue.Enqueue(request);
        if (!_running)
        {
            _running = true;
            _ = RunQueueAsync();
        }
        return request.Completion.Task;
    }

    private async Task RunQueueAsync()
    {
        try
        {
            // Collect formulas created by one native Markdown layout into the same capture.
            await Task.Delay(16);
            _initialization ??= InitializeAsync();
            await _initialization;
            while (!_disposed && (_queue.Count > 0 || _retryQueue.Count > 0))
            {
                var batch = new List<Request>(8);
                if (_retryQueue.Count > 0) batch.Add(_retryQueue.Dequeue());
                else while (batch.Count < 8 && _queue.Count > 0) batch.Add(_queue.Dequeue());
                await RenderBatchAsync(batch);
            }
        }
        catch
        {
            // A missing runtime or a stalled browser must leave readable source, not stuck tasks.
            _retryAfter = DateTime.UtcNow.AddSeconds(15);
            ResetView();
            foreach (var request in _pending.Values) request.Completion.TrySetResult(null);
            _pending.Clear();
            _queue.Clear();
            _retryQueue.Clear();
        }
        finally { _running = false; }
    }

    private async Task InitializeAsync()
    {
        if (_disposed) throw new ObjectDisposedException(nameof(KatexFormulaRenderer));
        string resources = Path.Combine(AppContext.BaseDirectory, "Resources", "Math");
        if (!File.Exists(Path.Combine(resources, "renderer.html"))) throw new FileNotFoundException("Local formula resources missing");
        _canvas = new Canvas { Width = 0, Height = 0, Opacity = 0, IsHitTestVisible = false,
            HorizontalAlignment = HorizontalAlignment.Left, VerticalAlignment = VerticalAlignment.Top };
        _view = new WebView2 { Width = 4096, Height = 2048, IsHitTestVisible = false, IsTabStop = false,
            DefaultBackgroundColor = Colors.Transparent };
        AutomationProperties.SetAccessibilityView(_view, AccessibilityView.Raw);
        var loaded = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        _view.Loaded += (_, _) => loaded.TrySetResult();
        _canvas.Children.Add(_view);
        _host.Children.Add(_canvas);
        if (_view.IsLoaded) loaded.TrySetResult();
        await loaded.Task.WaitAsync(TimeSpan.FromSeconds(5));
        string browserCache = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "KYNXA", "Cache", "MathWebView2");
        var environment = await CoreWebView2Environment.CreateWithOptionsAsync(null, browserCache, null).AsTask().WaitAsync(TimeSpan.FromSeconds(12));
        await _view.EnsureCoreWebView2Async(environment).AsTask().WaitAsync(TimeSpan.FromSeconds(12));
        var core = _view.CoreWebView2;
        core.Settings.AreDevToolsEnabled = false;
        core.Settings.AreDefaultContextMenusEnabled = false;
        core.Settings.AreBrowserAcceleratorKeysEnabled = false;
        core.Settings.IsStatusBarEnabled = false;
        core.Settings.IsWebMessageEnabled = true;
        core.SetVirtualHostNameToFolderMapping(HostName, resources, CoreWebView2HostResourceAccessKind.DenyCors);
        core.NavigationStarting += (_, e) => { if (!IsLocalResource(e.Uri)) e.Cancel = true; };
        core.NewWindowRequested += (_, e) => e.Handled = true;
        core.PermissionRequested += (_, e) => e.State = CoreWebView2PermissionState.Deny;
        core.AddWebResourceRequestedFilter("*", CoreWebView2WebResourceContext.All);
        core.WebResourceRequested += (_, e) =>
        {
            if (!IsLocalResource(e.Request.Uri)) e.Response = environment.CreateWebResourceResponse(null, 403, "Forbidden", "");
        };
        core.WebMessageReceived += (_, e) =>
        {
            try
            {
                var response = JsonSerializer.Deserialize<BatchResponse>(e.WebMessageAsJson, JsonOptions);
                if (response?.Type == "rendered" && response.Id == _responseId) _response?.TrySetResult(response);
            }
            catch (JsonException) { }
        };
        var navigated = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        core.NavigationCompleted += (_, e) =>
        {
            if (e.IsSuccess) navigated.TrySetResult();
            else navigated.TrySetException(new InvalidOperationException("Formula page failed to load"));
        };
        core.Navigate($"https://{HostName}/renderer.html");
        await navigated.Task.WaitAsync(TimeSpan.FromSeconds(8));
    }

    private static bool IsLocalResource(string uri) => Uri.TryCreate(uri, UriKind.Absolute, out var parsed)
        && parsed.Scheme == "https" && parsed.Host == HostName;

    private async Task RenderBatchAsync(List<Request> batch)
    {
        _responseId = Guid.NewGuid().ToString("N");
        _response = new(TaskCreationOptions.RunContinuationsAsynchronously);
        var items = batch.Select(request => new { latex = request.Key.Latex, block = request.Key.Block });
        string script = $"window.renderFormulas({JsonSerializer.Serialize(_responseId)}, {JsonSerializer.Serialize(items)})";
        try
        {
            await _view!.ExecuteScriptAsync(script).AsTask().WaitAsync(TimeSpan.FromSeconds(5));
            var response = await _response.Task.WaitAsync(TimeSpan.FromSeconds(8));
            using var stream = new InMemoryRandomAccessStream();
            await _view.CoreWebView2.CapturePreviewAsync(CoreWebView2CapturePreviewImageFormat.Png, stream).AsTask().WaitAsync(TimeSpan.FromSeconds(5));
            stream.Seek(0);
            using var bitmap = SKBitmap.Decode(stream.AsStreamForRead());
            if (bitmap is null || response.ViewportWidth <= 0 || response.ViewportHeight <= 0)
                throw new InvalidOperationException("Formula capture has no dimensions");
            double scaleX = bitmap.Width / response.ViewportWidth, scaleY = bitmap.Height / response.ViewportHeight;
            for (int i = 0; i < batch.Count; i++)
            {
                var request = batch[i];
                var bounds = response.Results.FirstOrDefault(result => result.Index == i);
                if (bounds?.Error == "batch-full" && !request.Retried)
                {
                    request.Retried = true;
                    _retryQueue.Enqueue(request);
                    continue;
                }
                FormulaImage? formula = null;
                if (bounds is { Error: null } && ValidBounds(bounds))
                {
                    int left = Math.Max(0, (int)Math.Floor(bounds.X * scaleX));
                    int top = Math.Max(0, (int)Math.Floor(bounds.Y * scaleY));
                    int right = Math.Min(bitmap.Width, (int)Math.Ceiling((bounds.X + bounds.Width) * scaleX));
                    int bottom = Math.Min(bitmap.Height, (int)Math.Ceiling((bounds.Y + bounds.Height) * scaleY));
                    using var crop = new SKBitmap();
                    if (right > left && bottom > top && bitmap.ExtractSubset(crop, new SKRectI(left, top, right, bottom)))
                    {
                        using var image = SKImage.FromBitmap(crop);
                        using var png = image.Encode(SKEncodedImageFormat.Png, 100);
                        formula = new(png.ToArray(), bounds.Width / 2, bounds.Height / 2, bounds.Baseline / 2);
                        AddToCache(request.Key, formula);
                    }
                }
                _pending.Remove(request.Key);
                request.Completion.TrySetResult(formula);
            }
        }
        finally { _response = null; _responseId = null; }
    }

    private static bool ValidBounds(FormulaBounds bounds) => double.IsFinite(bounds.Width) && double.IsFinite(bounds.Height)
        && double.IsFinite(bounds.X) && double.IsFinite(bounds.Y) && double.IsFinite(bounds.Baseline)
        && bounds.Width > 0 && bounds.Width <= 4096 && bounds.Height > 0 && bounds.Height <= 2048
        && bounds.X >= 0 && bounds.Y >= 0 && bounds.Baseline >= 0 && bounds.Baseline <= bounds.Height;

    private void AddToCache(FormulaKey key, FormulaImage formula)
    {
        if (formula.Png.LongLength > CacheLimit) return;
        while (_cacheOrder.Count > 0 && (_cache.Count >= 256 || _cacheBytes + formula.Png.LongLength > CacheLimit))
        {
            var oldest = _cacheOrder.Dequeue();
            if (_cache.Remove(oldest, out var removed)) _cacheBytes -= removed.Png.LongLength;
        }
        _cache.Add(key, formula);
        _cacheOrder.Enqueue(key);
        _cacheBytes += formula.Png.LongLength;
    }

    private void ResetView()
    {
        _response?.TrySetCanceled();
        _response = null;
        _responseId = null;
        try { _view?.Close(); } catch { }
        if (_canvas is not null) _host.Children.Remove(_canvas);
        _view = null;
        _canvas = null;
        _initialization = null;
    }

    private void HostUnloaded(object sender, RoutedEventArgs e)
    {
        _disposed = true;
        _host.Unloaded -= HostUnloaded;
        Workers.Remove(_xamlRoot);
        ResetView();
        foreach (var request in _pending.Values) request.Completion.TrySetResult(null);
        _pending.Clear();
        _queue.Clear();
        _retryQueue.Clear();
        _cache.Clear();
        _cacheOrder.Clear();
        _cacheBytes = 0;
    }
}
