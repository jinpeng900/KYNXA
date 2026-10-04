using KYNXA_Desktop.Controls;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;

namespace MarkdownUiSmoke;

public partial class App
{
    private void RunPhysicsMathChecks()
    {
        File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-physics-layout.txt"), "");
        const int formulaCount = 14;
        const string fixture = """
            角频率：$\omega=2\pi f$。

            转动定律：$\tau=I\alpha$。

            感应电动势：$\varepsilon=-\frac{d\Phi_B}{dt}$。

            声强级：$\beta=10\log_{10}\frac{I}{I_0}$。

            $$
            \omega=2\pi f
            $$

            $$
            \tau=I\alpha
            $$

            $$
            \varepsilon=-\frac{d\Phi_B}{dt}
            $$

            $$
            \beta=10\log_{10}\frac{I}{I_0}
            $$

            $$
            \frac{pV}{T}=\text{常量}
            $$

            $$
            f(x)=\begin{cases}x^2&x\ge0\\-x&x<0\end{cases}
            $$

            $$
            A=\begin{pmatrix}1&2\\3&4\end{pmatrix}
            $$

            $$
            \boxed{E=mc^2}
            $$

            $$
            a\overset{\text{def}}{=}b
            $$

            $$
            \underbrace{1+\cdots+1}_{n}=n
            $$
            """;
        var streamed = new MarkdownReply { IsStreaming = true };
        var finished = new MarkdownReply { Text = fixture };
        var panel = new StackPanel { Width = 680, Spacing = 24, Margin = new Thickness(20) };
        panel.Children.Add(streamed); panel.Children.Add(finished);
        _window = new Window { Title = "KYNXA Physics math regression", Content = new ScrollViewer { Content = panel } };
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(800, 900));
        panel.Loaded += async (_, _) =>
        {
            try
            {
                for (int count = 1; count < fixture.Length; count += 7)
                {
                    streamed.Text = fixture[..count];
                    await Task.Delay(5);
                }
                streamed.Text = fixture;
                streamed.IsStreaming = false;
                await Task.WhenAll(WaitForPhysicsImages(streamed, formulaCount), WaitForPhysicsImages(finished, formulaCount));
                foreach (double width in new[] { 680d, 320d, 680d })
                {
                    panel.Width = width;
                    await Task.Delay(250);
                    foreach (var reply in new[] { streamed, finished })
                    {
                        var formulas = (Canvas)((Grid)reply.Content).Children[2];
                        File.AppendAllText(Path.Combine(Path.GetTempPath(), "kynxa-physics-layout.txt"),
                            $"Width={width}; reply={reply == streamed}\n" + PhysicsGeometry(reply) + "\n");
                        Check(formulas.Children.Count == formulaCount, "physics and extended KaTeX formulas render");
                        Check(formulas.Children.OfType<Image>().All(image => image.Visibility == Visibility.Visible),
                            "basic physics formulas stay visible at width " + width);
                    }
                    Check(Geometry(streamed) == Geometry(finished), "physics streamed/static geometry matches at width " + width);
                }
                Check(ReadDocument(streamed.Document).Contains(@"\varepsilon=-\frac{d\Phi_B}{dt}"), "physics formula source remains selectable without rewriting");
                File.WriteAllText(_result, "PASS: four original physics formulas inline/display, Chinese text, cases/matrix/boxed/overset/underbrace, static/streaming parity, narrow and wide layouts, source selection.");
            }
            catch (Exception error) { File.WriteAllText(_result, "FAIL: " + error); }
            finally { if (!Environment.GetCommandLineArgs().Contains("--keep-open")) _window?.Close(); }
        };
        _window.Activate();
    }

    private static async Task WaitForPhysicsImages(MarkdownReply reply, int expected)
    {
        // Wait for every queued formula (including unsupported ones) before counting images.
        // 统计图片前等待所有排队公式完成，包括不支持的公式。
        await WaitForFormulaRendering(reply);
        var layer = (Canvas)((Grid)reply.Content).Children[2];
        if (layer.Children.Count == expected && layer.Children.OfType<Image>().All(image => image.Source is not null)) return;
        File.AppendAllText(Path.Combine(Path.GetTempPath(), "kynxa-physics-layout.txt"), PhysicsGeometry(reply) + "\n");
        Check(false, $"expected {expected} physics/extended formula images, received {layer.Children.Count}");
    }

    private static string PhysicsGeometry(MarkdownReply reply) => System.Text.Json.JsonSerializer.Serialize(new
    {
        images = ((Canvas)((Grid)reply.Content).Children[2]).Children.OfType<Image>().Select(image => new
        { visibility = image.Visibility.ToString(), width = image.Width, x = Canvas.GetLeft(image), y = Canvas.GetTop(image) }),
        runs = reply.Document.Blocks.OfType<Paragraph>().SelectMany(paragraph => paragraph.Inlines.OfType<Run>())
            .Where(run => run.Text.StartsWith('$')).Select(run => new
            {
                run.Text, run.CharacterSpacing,
                first = run.ContentStart.GetCharacterRect(LogicalDirection.Forward).ToString(),
                last = run.ContentEnd.GetCharacterRect(LogicalDirection.Backward).ToString(),
            }),
    });
}
