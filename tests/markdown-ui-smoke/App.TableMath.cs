using KYNXA_Desktop.Controls;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;

namespace MarkdownUiSmoke;

public partial class App
{
    private void RunTableMathChecks()
    {
        const string fixture = """
            表格前文，保留连续选择。

            ## 经典力学

            | 名称 | 公式 | 符号解释 | 说明/条件 |
            | --- | --- | --- | --- |
            | **速度** | \(v=\frac{s}{t}\) | \(v\)：速度，\(s\)：位移，\(t\)：时间 | 匀速直线运动 |
            | 加速度 | \(a=\frac{\Delta v}{\Delta t}\) | \(a\)：加速度，\(\Delta v\)：速度变化 | 平均加速度定义 |
            | 匀加速速度 | \(v=v_0+at\) | \(v_0\)：初速度 | 匀加速直线运动 |
            | 牛顿第二定律 | \(\vec F=m\vec a\) | \(\vec F\)：合力，\(m\)：质量 | 宏观低速惯性系 |
            | 伯努利方程 | \(P+\frac12\rho v^2+\rho gh=\text{常量}\) | 各项为压强和能量密度 | 理想不可压缩流体 |
            | 空公式 | | `sample.py` | [说明](https://example.com) |

            表格后文，复制不得丢失公式或其他单元格。
            """;
        var streamed = new MarkdownReply { IsStreaming = true };
        var finished = new MarkdownReply { Text = fixture };
        var panel = new StackPanel { Width = 840, Spacing = 24, Margin = new Thickness(24) };
        panel.Children.Add(streamed); panel.Children.Add(finished);
        _window = new Window { Title = "KYNXA Table formula regression", Content = new ScrollViewer { Content = panel } };
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(980, 950));
        panel.Loaded += async (_, _) =>
        {
            try
            {
                for (int count = 1; count < fixture.Length; count += 17)
                {
                    streamed.Text = fixture[..count];
                    await Task.Delay(5);
                }
                streamed.Text = fixture;
                streamed.IsStreaming = false;
                await WaitForFormulaRendering(streamed, finished);
                foreach (double width in new[] { 840d, 320d, 840d })
                {
                    panel.Width = width;
                    await Task.Delay(400);
                    foreach (var reply in new[] { streamed, finished })
                    {
                        var formulas = (Canvas)((Grid)reply.Content).Children[2];
                        Check(formulas.Children.Count == 13, "every formula and symbol in table cells renders");
                        Check(formulas.Children.OfType<Image>().All(image => image.Visibility == Visibility.Visible),
                            "table math remains visible at width " + width);
                        Check(formulas.Children.OfType<Image>().All(image => Canvas.GetLeft(image) >= 0 && Canvas.GetLeft(image) + image.Width <= width + 2),
                            "table math does not overflow the reply");
                        string copy = ReadDocument(reply.Document);
                        Check(copy.Contains(@"\frac{s}{t}") && copy.Contains(@"\text{常量}") && copy.Contains("sample.py") && copy.Contains("说明"),
                            "table copy retains formula source, code and link text");
                        Check(copy.IndexOf("表格前文", StringComparison.Ordinal) < copy.IndexOf(@"\frac{s}{t}", StringComparison.Ordinal)
                            && copy.IndexOf(@"\frac{s}{t}", StringComparison.Ordinal) < copy.IndexOf("表格后文", StringComparison.Ordinal),
                            "selection order crosses prose, table and following prose");
                        if (width > 800) CheckAlignedTableColumns(reply);
                    }
                    Check(Geometry(streamed) == Geometry(finished), "table final streamed/static geometry matches at width " + width);
                }
                var tableParagraph = streamed.Document.Blocks.OfType<Paragraph>().First(p => p.Inlines.OfType<Span>().Count() == 4);
                streamed.Document.Select(tableParagraph.ContentStart, tableParagraph.ContentEnd);
                string selected = streamed.Document.SelectedText;
                panel.Width = 320;
                await Task.Delay(200);
                Check(streamed.Document.SelectedText == selected, "resize does not replace a selected table");
                streamed.Document.Select(streamed.Document.ContentStart, streamed.Document.ContentStart);
                await Task.Delay(400);
                Check(ReadDocument(streamed.Document).Contains("名称："), "resize-only deferred layout resumes after deselection");
                panel.Width = 840;
                await Task.Delay(350);
                CheckAlignedTableColumns(streamed);
                streamed.Document.Select(tableParagraph.ContentStart, tableParagraph.ContentEnd);
                selected = streamed.Document.SelectedText;
                streamed.Text += "\n\n后续追加内容";
                await Task.Delay(150);
                Check(streamed.Document.SelectedText == selected, "table selection survives appended content");
                streamed.Document.Select(streamed.Document.ContentStart, streamed.Document.ContentStart);
                await Task.Delay(250);
                await WaitForFormulaRendering(streamed);
                Check(ReadDocument(streamed.Document).Contains("后续追加内容"), "updates resume after table selection clears");
                string manyFormulas = "| 名称 | 公式 | 符号解释 | 条件 |\n| --- | --- | --- | --- |\n"
                    + string.Join("\n", Enumerable.Range(1, 50).Select(index =>
                        $"| 公式 {index} | $v_{{{index}}}=\\frac{{s}}{{t}}$ | $s_{{{index}}}$ 位移，$t_{{{index}}}$ 时间 | 匀速运动 |"));
                streamed.Text = manyFormulas;
                await WaitForFormulaRendering(streamed);
                await Task.Delay(250);
                var manyImages = ((Canvas)((Grid)streamed.Content).Children[2]).Children.OfType<Image>().ToArray();
                Check(manyImages.Length == 150 && manyImages.All(image => image.Visibility == Visibility.Visible),
                    "all 150 formulas in a long table remain rendered");
                Check(ReadDocument(streamed.Document).Contains("$t_{50}$"), "copy includes the last formula in a long table");
                streamed.Text = fixture;
                await WaitForFormulaRendering(streamed);
                File.WriteAllText(_result, "PASS: table formulas/symbols (including 150-formula table), rich/empty cells, aligned columns, wide/narrow layouts, streaming parity, source copy across prose/table, selection preservation.");
            }
            catch (Exception error) { File.WriteAllText(_result, "FAIL: " + error); }
            finally { if (!Environment.GetCommandLineArgs().Contains("--keep-open")) _window?.Close(); }
        };
        _window.Activate();
    }

    private static void CheckAlignedTableColumns(MarkdownReply reply)
    {
        var rows = reply.Document.Blocks.OfType<Paragraph>()
            .Select(paragraph => paragraph.Inlines.OfType<Span>().ToArray()).Where(cells => cells.Length == 4).ToArray();
        Check(rows.Length == 7, "table rows keep rich native cell spans");
        for (int column = 0; column < 4; column++)
        {
            var starts = rows.Select(row => TableRuns(row[column]).FirstOrDefault(run => !string.IsNullOrWhiteSpace(run.Text)))
                .Where(run => run is not null).Select(run => run!.ContentStart.GetCharacterRect(LogicalDirection.Forward).X).ToArray();
            Check(starts.Max() - starts.Min() < 1.1, "table column " + column + " is aligned across rows");
        }
    }

    private static IEnumerable<Run> TableRuns(Span span)
    {
        foreach (var inline in span.Inlines)
            if (inline is Run run) yield return run;
            else if (inline is Span nested)
                foreach (var child in TableRuns(nested)) yield return child;
    }
}
