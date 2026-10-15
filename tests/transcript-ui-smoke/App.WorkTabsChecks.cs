using System.Diagnostics;
using System.Text.Json;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;

namespace TranscriptUiSmoke;

public partial class App
{
    private void LaunchWorkTabsChecks(Stopwatch cold)
    {
        foreach (string name in new[] { "Colors", "Dimensions", "Controls" })
            Resources.MergedDictionaries.Add(new ResourceDictionary { Source = new Uri($"ms-appx:///Styles/{name}.xaml") });
        var tabs = new ConversationWorkTabs { Width = 420, HorizontalAlignment = HorizontalAlignment.Left };
        var content = new StackPanel { Margin = new Thickness(20), Spacing = 12 };
        content.Children.Add(new TextBlock { Text = "KYNXA isolated workspace tabs · 标签栏", FontSize = 14 });
        content.Children.Add(tabs);
        _window = new Window { Title = "KYNXA isolated work tabs", Content = content };
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(1320, 260));
        _window.Closed += (_, _) => tabs.Dispose();
        tabs.Loaded += (_, _) => _ = RunWorkTabsChecksAsync(tabs, cold);
        _window.AppWindow.Show(activateWindow: false);
    }

    private async Task RunWorkTabsChecksAsync(ConversationWorkTabs tabs, Stopwatch cold)
    {
        File.WriteAllText(_result, "RUNNING: isolated workspace tab layout");
        try
        {
            var items = Enumerable.Range(1, 5).Select(index =>
                new ConversationWorkTab($"document/{index}", "document", $"实验 {index} · Physics.md")).ToArray();
            tabs.ShowConversation(Guid.NewGuid(), items);
            var scroll = WorkTabVisuals(tabs).OfType<ScrollViewer>().Single();
            var left = WorkTabVisuals(tabs).OfType<Button>().Single(button => button.Name == "WorkTabsScrollLeft");
            var right = WorkTabVisuals(tabs).OfType<Button>().Single(button => button.Name == "WorkTabsScrollRight");
            await WaitForWorkTabLayoutAsync(() => scroll.ScrollableWidth > 1 && left.Visibility == Visibility.Visible
                && right.Visibility == Visibility.Visible && !left.IsEnabled && right.IsEnabled, "narrow tabs expose overflow navigation");
            var rows = WorkTabVisuals(tabs).OfType<Grid>().Where(row => row.Name == "WorkTabItem").ToArray();
            _metrics["narrowGeometry"] = new
            {
                tabs = WorkTabGeometry(tabs, tabs), scroll = WorkTabGeometry(scroll, tabs),
                scroll.ViewportWidth, scroll.ViewportHeight, scroll.ExtentWidth, scroll.ExtentHeight, scroll.HorizontalOffset,
                rows = rows.Select(row =>
                {
                    var select = row.Children.OfType<Button>().Single(button => button.Name == "WorkTabSelect");
                    var close = row.Children.OfType<Button>().Single(button => button.Name == "WorkTabClose");
                    return new { key = row.Tag, row = WorkTabGeometry(row, tabs), title = WorkTabGeometry((TextBlock)select.Content, row),
                        select = WorkTabGeometry(select, row), close = WorkTabGeometry(close, row) };
                }).ToArray()
            };
            File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"),
                JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
            string preview = Path.Combine(Path.GetTempPath(), "kynxa-work-tabs.png");
            await CaptureWorkTabsAsync(tabs, preview);
            bool titlesFit = rows.Length == items.Length && rows.All(row =>
            {
                var select = row.Children.OfType<Button>().Single(button => button.Name == "WorkTabSelect");
                var title = (TextBlock)select.Content;
                double top = title.TransformToVisual(row).TransformPoint(new(0, 0)).Y;
                double leftEdge = title.TransformToVisual(select).TransformPoint(new(0, 0)).X;
                return Math.Abs(row.ActualHeight - 26) < 0.5
                    && Math.Abs(select.ActualWidth - select.Padding.Left - select.Padding.Right - 190) < 0.5
                    && Math.Abs(title.Width - 190) < 0.5 && Math.Abs(title.DesiredSize.Width - 190) < 0.5
                    && leftEdge >= select.Padding.Left - 0.5
                    && leftEdge + title.ActualWidth <= select.ActualWidth - select.Padding.Right + 0.5
                    && top >= -0.5 && top + title.ActualHeight <= row.ActualHeight + 0.5;
            });
            Check(Math.Abs(tabs.ActualHeight - 26) < 0.5 && titlesFit,
                "five real WinUI tab rows keep 26px height and 190px title slots without text clipping");
            Check(scroll.HorizontalScrollBarVisibility == ScrollBarVisibility.Hidden && scroll.HorizontalScrollMode == ScrollMode.Enabled
                && Math.Abs(scroll.ActualHeight - 26) < 0.5, "the system horizontal bar stays hidden while horizontal scrolling remains enabled");
            Check(left.Visibility == Visibility.Visible && right.Visibility == Visibility.Visible && right.IsEnabled && !left.IsEnabled,
                "overflow shows compact navigation with the correct initial direction");

            InvokeQueueControl(right);
            await WaitForWorkTabLayoutAsync(() => scroll.HorizontalOffset > 1, "right arrow moves the actual ScrollViewer");
            double advancedOffset = scroll.HorizontalOffset;
            InvokeQueueControl(left);
            await WaitForWorkTabLayoutAsync(() => scroll.HorizontalOffset < 1, "left arrow returns the actual ScrollViewer");
            Check(advancedOffset > 1 && scroll.HorizontalOffset < 1, "both arrow actions scroll the real viewport");

            tabs.Select(items[^1].Key);
            await WaitForWorkTabLayoutAsync(() =>
            {
                var last = WorkTabVisuals(tabs).OfType<Grid>().Single(row => row.Name == "WorkTabItem" && row.Tag as string == items[^1].Key);
                double x = last.TransformToVisual(scroll).TransformPoint(new(0, 0)).X;
                return last.ActualWidth > 200 && scroll.HorizontalOffset > advancedOffset
                    && x >= -1 && x + last.ActualWidth <= scroll.ActualWidth + 1;
            }, "the selected final tab enters the visible viewport");
            Check(tabs.SelectedTab?.Key == items[^1].Key && scroll.HorizontalOffset > advancedOffset,
                "selecting the final tab reveals it after normal asynchronous layout");
            await CaptureWorkTabsAsync(tabs, preview);

            tabs.Width = 1250;
            await WaitForWorkTabLayoutAsync(() => scroll.ScrollableWidth < 1 && left.Visibility == Visibility.Collapsed
                && right.Visibility == Visibility.Collapsed, "wide tabs remove overflow navigation");
            Check(left.Visibility == Visibility.Collapsed && right.Visibility == Visibility.Collapsed && scroll.ScrollableWidth < 1,
                "arrows consume no width when every tab fits");
            _metrics["checks"] = _checks; _metrics["mode"] = "work-tabs"; _metrics["durationMs"] = cold.ElapsedMilliseconds;
            _metrics["preview"] = preview; _metrics["narrowArrowOffset"] = advancedOffset;
            File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics));
            File.WriteAllText(_result, $"PASS: {_checks} isolated WinUI work-tab checks; 26px rows, 190px titles, hidden system bar, arrow scrolling, selected-tab visibility and no-overflow collapse. Preview: kynxa-work-tabs.png.");
        }
        catch (Exception error) { File.WriteAllText(_result, "FAIL: " + error); }
        finally { _window?.Close(); }
    }

    private static object WorkTabGeometry(FrameworkElement element, UIElement relativeTo)
    {
        var position = element.TransformToVisual(relativeTo).TransformPoint(new(0, 0));
        return new { element.Name, actual = new { width = element.ActualWidth, height = element.ActualHeight },
            desired = new { width = element.DesiredSize.Width, height = element.DesiredSize.Height },
            bounds = new { x = position.X, y = position.Y, width = element.ActualWidth, height = element.ActualHeight } };
    }

    private static IEnumerable<FrameworkElement> WorkTabVisuals(DependencyObject parent)
    {
        for (int index = 0; index < VisualTreeHelper.GetChildrenCount(parent); index++)
        {
            var child = VisualTreeHelper.GetChild(parent, index);
            if (child is FrameworkElement element) yield return element;
            foreach (var descendant in WorkTabVisuals(child)) yield return descendant;
        }
    }

    private static async Task WaitForWorkTabLayoutAsync(Func<bool> completed, string description)
    {
        DateTime deadline = DateTime.UtcNow.AddSeconds(5);
        while (!completed())
        {
            if (DateTime.UtcNow >= deadline) throw new InvalidOperationException(description + " (timed out)");
            await Task.Delay(25);
        }
    }

    private static async Task CaptureWorkTabsAsync(FrameworkElement tabs, string path)
    {
        // Capture this isolated control only; no desktop pixels or other applications are read.
        // 只渲染隔离测试控件，不读取桌面像素或其他应用。
        var image = new RenderTargetBitmap();
        await image.RenderAsync(tabs);
        using var pixels = DataReader.FromBuffer(await image.GetPixelsAsync());
        byte[] bgra = new byte[pixels.UnconsumedBufferLength]; pixels.ReadBytes(bgra);
        using var stream = new InMemoryRandomAccessStream();
        var encoder = await BitmapEncoder.CreateAsync(BitmapEncoder.PngEncoderId, stream);
        encoder.SetPixelData(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Premultiplied,
            (uint)image.PixelWidth, (uint)image.PixelHeight, 96, 96, bgra);
        await encoder.FlushAsync();
        using var png = new DataReader(stream.GetInputStreamAt(0));
        await png.LoadAsync((uint)stream.Size);
        byte[] bytes = new byte[png.UnconsumedBufferLength]; png.ReadBytes(bytes);
        await File.WriteAllBytesAsync(path, bytes);
    }
}
