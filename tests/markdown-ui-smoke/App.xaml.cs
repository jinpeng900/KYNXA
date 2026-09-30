using KYNXA_Desktop.Controls;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;

namespace MarkdownUiSmoke;

public partial class App : Application
{
    private Window? _window;
    private readonly string _result = Path.Combine(Path.GetTempPath(), "kynxa-markdown-smoke.txt");

    public App()
    {
        InitializeComponent();
        UnhandledException += (_, e) => File.WriteAllText(_result, "FAIL: " + e.Exception);
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        if (Environment.GetCommandLineArgs().Contains("--table-math"))
        {
            RunTableMathChecks();
            return;
        }
        if (Environment.GetCommandLineArgs().Contains("--table-probe"))
        {
            RunTableProbe();
            return;
        }
        if (Environment.GetCommandLineArgs().Contains("--physics-math"))
        {
            RunPhysicsMathChecks();
            return;
        }
        if (Environment.GetCommandLineArgs().Contains("--delimiter-math"))
        {
            RunMathDelimiterChecks();
            return;
        }
        if (Environment.GetCommandLineArgs().Contains("--streaming"))
        {
            RunStreamingChecks();
            return;
        }
        if (Environment.GetCommandLineArgs().Contains("--layout-parity"))
        {
            RunStreamingLayoutChecks();
            return;
        }
        if (Environment.GetCommandLineArgs().Contains("--math"))
        {
            var math = new MarkdownReply { Margin = new Thickness(24), Text = """
                # 公式渲染
                皮亚诺算术：\(1+1=S(1)=2\)。普通中文与行内公式 $x^2+y^2=z^2$ 连续排版。

                集合：$0=\varnothing$，$1=\{\varnothing\}$，$1+1\cong 2$。

                \[
                \frac{a+b}{c}=\sqrt{x^2+y^2}
                \]

                $$
                \sum_{i=1}^{n}i=\frac{n(n+1)}{2}
                $$

                [
                1+1=0
                ]

                行内分数 $\frac{1}{2}$ 与根号 $\sqrt{2}$，以及矩阵 $\begin{pmatrix}a&b\\c&d\end{pmatrix}$。

                不支持的公式保留源码：$\unknowncommand{x}$。

                ```python
                print(r"\(不要渲染代码内的公式\)")
                ```
                """ };
            var scrollMath = new ScrollViewer { Content = math };
            _window = new Window { Title = "KYNXA Math UI smoke", Content = scrollMath };
            _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(900, 800));
            math.Loaded += (_, _) =>
            {
                math.Document.SelectAll();
                Check(math.Document.SelectedText.Contains(@"\frac{a+b}{c}"), "formula source selectable");
                math.Document.Select(math.Document.ContentStart, math.Document.ContentStart);
                AutomationProperties.SetAutomationId(math.Document, "MathDocument");
                File.WriteAllText(_result, "PASS: formula source remains in native selectable document.");
            };
            _window.Activate();
            return;
        }
        if (Environment.GetCommandLineArgs().Contains("--conversation"))
        {
            var messages = new List<MarkdownReply>();
            var list = new ListView { SelectionMode = ListViewSelectionMode.None, Margin = new Thickness(120, 12, 16, 94),
                ItemsPanel = (ItemsPanelTemplate)Microsoft.UI.Xaml.Markup.XamlReader.Load(
                    "<ItemsPanelTemplate xmlns='http://schemas.microsoft.com/winfx/2006/xaml/presentation'><StackPanel /></ItemsPanelTemplate>") };
            list.ItemContainerStyle = new Style(typeof(ListViewItem))
            {
                Setters = { new Setter(Control.HorizontalContentAlignmentProperty, HorizontalAlignment.Stretch),
                    new Setter(Control.PaddingProperty, new Thickness(0)), new Setter(Control.IsTabStopProperty, false) },
            };
            AutomationProperties.SetAutomationId(list, "ConversationScroll");
            for (int i = 0; i < 20; i++)
            {
                bool user = i % 2 == 0;
                var message = new MarkdownReply { IsPlainText = user, FontSize = user ? 16 : 14,
                    Text = user ? $"USER_{i:00} 用户提问：保留 **原始文本**，支持跨消息选择。"
                        : $"ASSISTANT_{i:00} 模型回复：**加粗文字**与代码可以连续选中。\n\n```python\nvalue = {i}  # 中文注释\n```",
                    Margin = new Thickness(user ? 140 : 40, 8, 0, 8) };
                AutomationProperties.SetAutomationId(message.Document, $"Message{i}");
                messages.Add(message);
                list.Items.Add(message);
            }
            var status = new TextBlock { MaxLines = 1, VerticalAlignment = VerticalAlignment.Bottom, Margin = new Thickness(16) };
            AutomationProperties.SetAutomationId(status, "SelectedConversation");
            var selectionState = new TextBlock { Margin = new Thickness(8), VerticalAlignment = VerticalAlignment.Top };
            AutomationProperties.SetAutomationId(selectionState, "SelectionState");
            var timer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(100) };
            timer.Tick += (_, _) =>
            {
                status.Text = messages[0].SelectedConversationText;
                selectionState.Text = !TextSelectionAutoScroll.HasActiveSelection
                    && messages.All(message => message.Document.TextHighlighters.All(highlight => highlight.Ranges.Count == 0)) ? "idle" : "selected";
            };
            var content = new Grid { Background = new SolidColorBrush(Colors.White) };
            var sidebarButton = new Button { Content = "侧栏", Margin = new Thickness(8, 50, 8, 0), VerticalAlignment = VerticalAlignment.Top };
            AutomationProperties.SetAutomationId(sidebarButton, "SidebarAction");
            var sidebar = new Grid { Width = 104, HorizontalAlignment = HorizontalAlignment.Left, Background = new SolidColorBrush(Colors.WhiteSmoke) };
            sidebar.Children.Add(sidebarButton); sidebar.Children.Add(selectionState);
            var input = new TextBox { PlaceholderText = "输入消息", Margin = new Thickness(132, 0, 24, 44), VerticalAlignment = VerticalAlignment.Bottom };
            AutomationProperties.SetAutomationId(input, "ConversationInput");
            content.Children.Add(sidebar); content.Children.Add(list); content.Children.Add(input); content.Children.Add(status);
            _window = new Window { Title = "KYNXA Conversation selection smoke", Content = content };
            _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(1000, 850));
            _window.Closed += (_, _) => timer.Stop();
            timer.Start();
            _window.Activate();
            return;
        }
        const string fixture = """
            # Markdown 回复预览

            中文正文支持 **加粗**、*斜体*、~~删除线~~，以及 `inline_code()`。A &amp; B 😊

            ## 项目步骤
            1. 创建项目
            2. 配置连接
               - 本地模型
               - 云端模型

            > 引用内容也能连续选中复制。

            ```python
            # 输出问候语
            def greet(name):
                print(f"你好，{name}")
                return {"ok": True}
            ```

            - [x] 支持 Markdown
            - [ ] 验证拖选自动滚动

            | 名称 | 状态 |
            | --- | --- |
            | 本地 Qwen | 已连接 |
            | API | 未配置 |

            [普通链接](https://example.com) 和 [不可执行的链接](javascript:alert(1))。

            <script>alert('只作为文字显示')</script>

            ---
            """;
        var reply = new MarkdownReply { Text = fixture, Margin = new Thickness(0, 5, 0, 0) };
        var scroll = new ListView { Margin = new Thickness(0, 36, 0, 50), SelectionMode = ListViewSelectionMode.None,
            Padding = new Thickness(12), Background = new SolidColorBrush(Colors.Transparent) };
        scroll.ItemContainerStyle = new Style(typeof(ListViewItem))
        {
            Setters = { new Setter(Control.HorizontalContentAlignmentProperty, HorizontalAlignment.Stretch),
                new Setter(Control.PaddingProperty, new Thickness(0)), new Setter(Control.IsTabStopProperty, false) },
        };
        scroll.Items.Add(new TextBlock { Text = "用户消息 · 验证实际聊天列表布局", Margin = new Thickness(0, 8, 0, 24) });
        var row = new Grid { ColumnSpacing = 10 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(32) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        var avatarImage = new Image { Width = 28, Height = 28, Source = new BitmapImage(new Uri("ms-appx:///logo.png")) };
        AutomationProperties.SetAutomationId(avatarImage, "ReplyAvatar");
        AutomationProperties.SetName(avatarImage, "助手图标");
        var avatar = new Border { Width = 32, Height = 32, CornerRadius = new CornerRadius(16), VerticalAlignment = VerticalAlignment.Top,
            Child = avatarImage };
        row.Children.Add(avatar);
        Grid.SetColumn(reply, 1);
        row.Children.Add(reply);
        scroll.Items.Add(row);
        scroll.Items.Add(new TextBlock { Text = "下一条回复 · 列表边界", Margin = new Thickness(42, 30, 0, 30) });
        AutomationProperties.SetAutomationId(scroll, "ReplyScroll");
        var root = new Grid { Background = new SolidColorBrush(Colors.White) };
        root.Children.Add(new TextBlock { Text = "KYNXA · 回复渲染与拖选测试", Margin = new Thickness(24, 8, 0, 0) });
        root.Children.Add(scroll);
        _window = new Window { Title = "KYNXA Markdown UI smoke", Content = root };
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(900, 700));
        reply.Loaded += (_, _) =>
        {
            try
            {
                var document = reply.Document;
                Check(document.Blocks.OfType<Paragraph>().First().FontSize == 23, "heading format");
                Check(document.Blocks.OfType<Paragraph>().Any(p => p.FontFamily.Source.Contains("Cascadia")), "code formatting");
                var code = document.Blocks.OfType<Paragraph>().First(p => p.FontFamily.Source.Contains("Cascadia"));
                Check(code.Inlines.OfType<Run>().Select(r => (r.Foreground as SolidColorBrush)?.Color).Distinct().Count() >= 4,
                    "code has distinct keyword, comment, string and plain colors");
                document.SelectAll();
                string selected = document.SelectedText;
                Check(selected.Contains("def greet(name):") && !selected.Contains("```"), "code fence removed, content preserved");
                Check(selected.Contains("加粗") && !selected.Contains("**加粗**"), "emphasis parsed");
                Check(selected.Contains("A & B 😊"), "entities and Unicode preserved");
                Check(selected.Contains("本地 Qwen") && selected.Contains("☑"), "table and tasks");
                Check(selected.Contains("<script>"), "HTML treated as inert text");
                var links = document.Blocks.OfType<Paragraph>().SelectMany(p => p.Inlines).OfType<Hyperlink>().ToArray();
                Check(links.Length == 1 && links[0].NavigateUri.Scheme == "https", "unsafe link rejected");
                document.Select(document.ContentStart, document.ContentStart);
                reply.Text = "临时内容";
                document.SelectAll();
                Check(document.SelectedText.Trim() == "临时内容", "rebound content clears old blocks");
                document.Select(document.ContentStart, document.ContentStart);
                reply.Text = "```python\nprint('未闭合的代码块')";
                document.SelectAll();
                Check(document.SelectedText.Contains("print('未闭合的代码块')"), "unfinished code fence");
                document.Select(document.ContentStart, document.ContentStart);
                reply.Text = fixture + "\n\n" + string.Join("\n\n", Enumerable.Range(1, 12)
                    .Select(i => $"第 {i:00} 段：用于验证长回复的跨段落拖选。按住鼠标移动到边缘，选区应持续向上或向下扩展。 **内容 {i:00}**。"))
                    + "\n\n```python\n" + string.Join("\n", Enumerable.Range(1, 160)
                        .Select(i => $"value_{i:000} = {{\"name\": \"你好 {i:000}\", \"enabled\": True}}  # 示例 {i:000}")) + "\n```";
                AutomationProperties.SetAutomationId(document, "ReplyDocument");
                File.WriteAllText(_result, "PASS: Markdown headings, emphasis, colored code, tasks, tables, safe links, HTML, selectable text, content replacement.");
                if (Environment.GetCommandLineArgs().Contains("--auto")) _window.Close();
            }
            catch (Exception error) { File.WriteAllText(_result, "FAIL: " + error); }
        };
        _window.Activate();
    }

    private static void Check(bool condition, string description)
    {
        if (!condition) throw new InvalidOperationException(description);
    }

    private void RunStreamingChecks()
    {
        var earlier = new MarkdownReply { Text = "先前的用户消息", IsPlainText = true };
        const string prefix = "第一段 **稳定内容** $1+1=2$。\n\n";
        var reply = new MarkdownReply { IsStreaming = true, Text = prefix + "第二段" };
        var panel = new StackPanel { Margin = new Thickness(24), Spacing = 16 };
        panel.Children.Add(earlier);
        panel.Children.Add(reply);
        _window = new Window { Title = "KYNXA Streaming markdown smoke", Content = new ScrollViewer { Content = panel } };
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(900, 700));
        reply.Loaded += async (_, _) =>
        {
            try
            {
                await WaitForFormulaRendering(reply);
                var document = reply.Document;
                var first = document.Blocks[0];
                var formulaCanvas = (Canvas)((Grid)reply.Content).Children[2];
                Check(formulaCanvas.Children.Count == 1, "initial inline formula rendered");
                var firstFormula = formulaCanvas.Children[0];
                for (int index = 0; index < 30; index++) reply.Text = prefix + "第二段" + new string('字', index + 1);
                Check(ReferenceEquals(first, document.Blocks[0]), "stable paragraph retained across deltas");
                Check(ReferenceEquals(firstFormula, formulaCanvas.Children[0]), "stable formula image retained across deltas");

                document.Select(first.ContentStart, first.ContentEnd);
                string frozen = document.SelectedText;
                Check(TextSelectionAutoScroll.HasActiveSelection, "native selection pauses auto-follow");
                reply.Text = prefix + "流式最后一段";
                reply.IsStreaming = false;
                reply.Text = prefix + "最终核对后的内容";
                Check(document.SelectedText == frozen, "selection retained through final reconciliation");
                Check(ReferenceEquals(first, document.Blocks[0]), "selected paragraph never rebuilt");
                document.Select(document.ContentStart, document.ContentStart);
                await Task.Delay(250);
                Check(ReadDocument(document).Contains("最终核对后的内容"), "latest deferred content resumes after deselection");

                earlier.Document.SelectAll();
                reply.IsStreaming = true;
                reply.Text = prefix + "另外消息选中时暂缓更新";
                Check(ReadDocument(document).Contains("最终核对后的内容"), "selection in earlier message protects conversation");
                earlier.Document.Select(earlier.Document.ContentStart, earlier.Document.ContentStart);
                await Task.Delay(250);
                Check(ReadDocument(document).Contains("另外消息选中时暂缓更新"), "conversation selection release resumes updates");

                reply.Text = "$$\n\\frac{1}{2}";
                Check(formulaCanvas.Children.Count == 0, "unfinished block formula stays readable source");
                Check(ReadDocument(document).Contains(@"\frac{1}{2}"), "unfinished formula content retained");
                reply.Text += "\n$$";
                await WaitForFormulaRendering(reply);
                Check(formulaCanvas.Children.Count == 1, "closed formula renders during stream");
                reply.Text = "```python\nprint('hello";
                Check(ReadDocument(document).Contains("print('hello"), "unfinished code fence remains readable");
                reply.Text += "')\n```";
                reply.IsStreaming = false;
                Check(ReadDocument(document).Contains("print('hello')"), "finished code content preserved");

                const string delayedLatex = @"\boxed{\sum_{i=1}^{8}i=36}";
                reply.Text = "选择中的新公式 $" + delayedLatex + "$。";
                document.SelectAll();
                string selectedSource = document.SelectedText;
                var worker = KYNXA_Desktop.Services.KatexFormulaRenderer.ForElement(reply)!;
                await worker.RenderAsync(delayedLatex, false);
                await Task.Delay(150);
                Check(document.SelectedText == selectedSource && formulaCanvas.Children.Count == 0,
                    "completed asynchronous formula does not change an active native selection");
                document.Select(document.ContentStart, document.ContentStart);
                await WaitForFormulaRendering(reply);
                Check(formulaCanvas.Children.Count == 1, "deferred formula image applies after selection clears");

                reply.Text = @"旧内容 $\boxed{999}$。";
                reply.Text = @"新内容 $\boxed{888}$。";
                await WaitForFormulaRendering(reply);
                await worker.RenderAsync(@"\boxed{999}", false);
                await Task.Delay(50);
                Check(formulaCanvas.Children.Count == 1 && ReadDocument(document).Contains(@"\boxed{888}"),
                    "a replaced formula cannot receive an older asynchronous image");
                reply.Text = "[链接][ref]";
                reply.Text += "\n\n[ref]: https://example.com";
                Check(document.Blocks.OfType<Paragraph>().SelectMany(p => p.Inlines).OfType<Hyperlink>().Any(),
                    "late reference definition updates earlier semantic block");
                File.WriteAllText(_result, "PASS: stable paragraphs/formulas, native and conversation selections, deferred final reconciliation, unfinished math/code, reference links.");
            }
            catch (Exception error) { File.WriteAllText(_result, "FAIL: " + error); }
            finally { _window?.Close(); }
        };
        _window.Activate();
    }

    private static string ReadDocument(RichTextBlock document)
    {
        document.SelectAll();
        string text = document.SelectedText;
        document.Select(document.ContentStart, document.ContentStart);
        return text;
    }

    private void RunStreamingLayoutChecks()
    {
        const string fixture = "这是流式输出的演示。\n\n## 结果\n\n文字会逐步出现，支持 **加粗** 和公式 $x^2+y^2=z^2$。\n\n```python\nprint(\"你好\")\n```\n\n| 项目 | 状态 |\n| --- | --- |\n| 正文 | 已完成 |";
        var streamed = new MarkdownReply { IsStreaming = true };
        var finished = new MarkdownReply { Text = fixture };
        var panel = new StackPanel { Width = 680, Spacing = 24 };
        panel.Children.Add(streamed); panel.Children.Add(finished);
        _window = new Window { Title = "KYNXA Streaming layout parity", Content = new ScrollViewer { Content = panel } };
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(900, 800));
        panel.Loaded += async (_, _) =>
        {
            try
            {
                for (int count = 1; count < fixture.Length; count += 12)
                {
                    streamed.Text = fixture[..count];
                    await Task.Delay(20);
                }
                streamed.Text = fixture;
                streamed.IsStreaming = false;
                await WaitForFormulaRendering(streamed, finished);
                var beforeResize = new { streamed = Geometry(streamed), finished = Geometry(finished) };
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-streaming-layout.json"), System.Text.Json.JsonSerializer.Serialize(beforeResize));
                foreach (var reply in new[] { streamed, finished })
                    Check(((Canvas)((Grid)reply.Content).Children[0]).Children.OfType<Border>()
                        .All(border => Math.Abs(border.Width - reply.ActualWidth) < 1), "final block backgrounds stretch to reply width");
                Check(Geometry(streamed) == Geometry(finished), "streamed final geometry equals static rendering");
                panel.Width = 480;
                await Task.Delay(250);
                Check(Geometry(streamed) == Geometry(finished), "streamed and static geometry remain equal after resize");
                foreach (var reply in new[] { streamed, finished })
                {
                    var backgrounds = (Canvas)((Grid)reply.Content).Children[0];
                    Check(backgrounds.Children.OfType<Border>().All(border => Math.Abs(border.Width - reply.ActualWidth) < 1),
                        "code and table backgrounds use the full final document width");
                }
                File.WriteAllText(_result, "PASS: streamed/static final Markdown geometry matches, formula positions match, code/table backgrounds fill reply width before and after resize.");
            }
            catch (Exception error) { File.WriteAllText(_result, "FAIL: " + error); }
            finally { _window?.Close(); }
        };
        _window.Activate();
    }

    private static string Geometry(MarkdownReply reply)
    {
        var surface = (Grid)reply.Content;
        return System.Text.Json.JsonSerializer.Serialize(new
        {
            controlWidth = Math.Round(reply.ActualWidth, 2), documentWidth = Math.Round(reply.Document.ActualWidth, 2),
            documentHeight = Math.Round(reply.Document.ActualHeight, 2),
            decorations = new[] { (Canvas)surface.Children[0], (Canvas)surface.Children[2] }
                .Select(canvas => canvas.Children.OfType<FrameworkElement>().Select(element => new
                {
                    x = Math.Round(Canvas.GetLeft(element), 2), y = Math.Round(Canvas.GetTop(element), 2),
                    width = Math.Round(element.Width, 2), height = Math.Round(element.Height, 2), visibility = element.Visibility.ToString(),
                }).ToArray()).ToArray(),
        });
    }
}
