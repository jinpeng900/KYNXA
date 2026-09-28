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
        if (Environment.GetCommandLineArgs().Contains("--conversation"))
        {
            var messages = new List<MarkdownReply>();
            var list = new ListView { SelectionMode = ListViewSelectionMode.None, Margin = new Thickness(16, 12, 16, 50),
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
            var timer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(100) };
            timer.Tick += (_, _) => status.Text = messages[0].SelectedConversationText;
            var content = new Grid { Background = new SolidColorBrush(Colors.White) };
            content.Children.Add(list); content.Children.Add(status);
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
                reply.Text = "临时内容";
                document.SelectAll();
                Check(document.SelectedText.Trim() == "临时内容", "rebound content clears old blocks");
                reply.Text = "```python\nprint('未闭合的代码块')";
                document.SelectAll();
                Check(document.SelectedText.Contains("print('未闭合的代码块')"), "unfinished code fence");
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
}
