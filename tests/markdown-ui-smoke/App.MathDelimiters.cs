using KYNXA_Desktop.Controls;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace MarkdownUiSmoke;

public partial class App
{
    private void RunMathDelimiterChecks()
    {
        const string fixture = """
            这里 \(a = S(0)\)，\(b = 0\)，所以：

            \[
            S(0) + S(0) = S\big(S(0) + 0\big)
            \]

            再应用加法定义：

            \[
            S(0) + 0 = S(0)
            \]

            代回去：

            \[
            S\big(S(0) + 0\big) = S\big(S(0)\big)
            \]

            `\big(代码保持原样\big)`
            """;
        const string modularFixture = """
            在模 2 运算中：

            $$1+1 \equiv 0 \pmod 2$$

            在模 3 运算中：

            $$1+1 \equiv 2 \pmod 3$$

            在模 1 运算中：

            $$1+1 \equiv 0 \pmod 1$$

            这说明 $1+1=2$ 依赖于我们选择的公理系统。在皮亚诺算术里，它就是 $2$。
            """;
        var streamed = new MarkdownReply { IsStreaming = true };
        var finished = new MarkdownReply { Text = fixture };
        var panel = new StackPanel { Width = 680, Spacing = 20, Margin = new Thickness(20) };
        panel.Children.Add(streamed); panel.Children.Add(finished);
        _window = new Window { Title = "KYNXA Math delimiter regression", Content = new ScrollViewer { Content = panel } };
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(800, 900));
        panel.Loaded += async (_, _) =>
        {
            try
            {
                foreach (var (sourceText, originalCommand) in new[] { (fixture, @"S\big(S(0) + 0\big)"), (modularFixture, @"1+1 \equiv 0 \pmod 2") })
                {
                    streamed.Text = string.Empty;
                    streamed.IsStreaming = true;
                    finished.Text = sourceText;
                    for (int count = 1; count < sourceText.Length; count += 9)
                    {
                        streamed.Text = sourceText[..count];
                        await Task.Delay(10);
                    }
                    streamed.Text = sourceText;
                    streamed.IsStreaming = false;
                    await WaitForFormulaRendering(streamed, finished);
                    foreach (var reply in new[] { streamed, finished })
                    {
                        var formulas = (Canvas)((Grid)reply.Content).Children[2];
                        Check(formulas.Children.Count == 5, "both inline and all three display formulas render: " + originalCommand);
                        Check(formulas.Children.OfType<Image>().All(image => image.Visibility == Visibility.Visible), "no supported formula falls back to raw dollar/LaTeX text");
                        Check(ReadDocument(reply.Document).Contains(originalCommand), "copy keeps original LaTeX commands");
                    }
                    await Task.Delay(100);
                    Check(Geometry(streamed) == Geometry(finished), "screenshot formulas match static layout after streaming");
                }
                foreach (string source in new[] { "$x^2$", "$$x^2$$", "\\(x^2\\)", "\\(\nx^2\n\\)", "\\[x^2\\]", "$$\nx^2\n$$", "```math\nx^2\n```" })
                {
                    streamed.Text = source;
                    await WaitForFormulaRendering(streamed);
                    Check(((Canvas)((Grid)streamed.Content).Children[2]).Children.Count == 1, "math syntax renders: " + source);
                }
                foreach (string code in new[] { "`\\big(x\\big)`", "`1+1 \\equiv 0 \\pmod 2`", "```tex\n\\[x^2\\]\n$$x$$\n```", "```python\nprint('$x$')\n```" })
                {
                    streamed.Text = code;
                    Check(((Canvas)((Grid)streamed.Content).Children[2]).Children.Count == 0, "ordinary code remains code");
                }
                streamed.IsStreaming = true;
                streamed.Text = "```math\n\\frac{1}{2}";
                Check(((Canvas)((Grid)streamed.Content).Children[2]).Children.Count == 0, "math fence waits for finalization");
                streamed.Text += "\n```";
                streamed.IsStreaming = false;
                await WaitForFormulaRendering(streamed);
                Check(((Canvas)((Grid)streamed.Content).Children[2]).Children.Count == 1, "finalized math fence renders without text change");
                File.WriteAllText(_result, "PASS: screenshot big-delimiter and modular-arithmetic formulas, static/streaming parity, inline/display/multiline math, math fences, original copy and code protection.");
            }
            catch (Exception error) { File.WriteAllText(_result, "FAIL: " + error); }
            finally { if (!Environment.GetCommandLineArgs().Contains("--keep-open")) _window?.Close(); }
        };
        _window.Activate();
    }

    private static async Task WaitForFormulaRendering(params MarkdownReply[] replies)
    {
        var deadline = DateTime.UtcNow.AddSeconds(20);
        while (replies.Any(reply => reply.HasPendingFormulaRendering) && DateTime.UtcNow < deadline)
            await Task.Delay(50);
        Check(replies.All(reply => !reply.HasPendingFormulaRendering), "formula rendering completes within the deadline");
        await Task.Delay(100); // Native text layout follows the completed image dimensions.
        // 原生文本布局应跟随已完成渲染的图片尺寸。
    }
}
