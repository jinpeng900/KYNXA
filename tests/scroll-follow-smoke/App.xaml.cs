using KYNXA_Desktop.Controls;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Markup;
using Microsoft.UI.Xaml.Media;

namespace ScrollFollowSmoke;

public partial class App : Application
{
    private Window? _window;
    private readonly string _result = Path.Combine(Path.GetTempPath(), "kynxa-scroll-follow-smoke.txt");
    private readonly List<string> _checks = [];

    public App()
    {
        InitializeComponent();
        UnhandledException += (_, e) => File.WriteAllText(_result, "FAIL: " + e.Exception);
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        File.WriteAllText(_result, "RUNNING");
        var messages = new ListView
        {
            SelectionMode = ListViewSelectionMode.None, Padding = new Thickness(20, 16, 20, 30),
            ItemsPanel = (ItemsPanelTemplate)XamlReader.Load(
                "<ItemsPanelTemplate xmlns='http://schemas.microsoft.com/winfx/2006/xaml/presentation'><StackPanel /></ItemsPanelTemplate>"),
            ItemContainerStyle = new Style(typeof(ListViewItem))
            {
                Setters = { new Setter(Control.HorizontalContentAlignmentProperty, HorizontalAlignment.Stretch),
                    new Setter(Control.PaddingProperty, new Thickness(0)), new Setter(Control.IsTabStopProperty, false) }
            }
        };
        _window = new Window { Title = "KYNXA Scroll Follow Smoke", Content = messages };
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(800, 520));
        messages.Loaded += async (_, _) =>
        {
            try { await Verify(messages); }
            catch (Exception exception) { File.WriteAllText(_result, string.Join('\n', _checks) + "\nFAIL: " + exception); }
            finally { _window.Close(); }
        };
        _window.Activate();
    }

    private async Task Verify(ListView messages)
    {
        messages.UpdateLayout();
        var scroll = FindChild<ScrollViewer>(messages) ?? throw new InvalidOperationException("ListView scroll viewer not found");
        var follow = new ConversationAutoFollow(scroll);
        RichTextBlock reply = new();
        void Chat(int count)
        {
            messages.Items.Clear();
            for (int i = 0; i < count; i++)
                messages.Items.Add(new TextBlock
                {
                    Text = $"历史消息 {i}: " + new string('文', 120), TextWrapping = TextWrapping.Wrap,
                    FontSize = 16, Margin = new Thickness(0, 8, 0, 8)
                });
            reply = new RichTextBlock { FontSize = 14, TextWrapping = TextWrapping.Wrap };
            AddParagraph("开始回复。");
            messages.Items.Add(reply);
        }
        void AddParagraph(string content) => reply.Blocks.Add(new Paragraph
        {
            Inlines = { new Run { Text = content } }, Margin = new Thickness(0, 0, 0, 12)
        });
        async Task Settle() { messages.UpdateLayout(); await Task.Delay(160); messages.UpdateLayout(); await Task.Delay(40); }
        void Bottom(string name) => Check(scroll.ScrollableHeight - scroll.VerticalOffset <= 2,
            $"{name}: offset={scroll.VerticalOffset:F1}, maximum={scroll.ScrollableHeight:F1}");
        void Stable(double offset, string name) => Check(Math.Abs(scroll.VerticalOffset - offset) <= 2,
            $"{name}: expected={offset:F1}, actual={scroll.VerticalOffset:F1}");

        Chat(20); follow.OpenChat(); await Settle(); Bottom("initial conversation opens at bottom");
        for (int i = 0; i < 12; i++)
        {
            AddParagraph($"增量 {i}: " + new string('回', 100));
            follow.ContentChanged(); await Settle(); Bottom($"growth {i} follows bottom");
        }
        // Rich-text block edits and width/viewport changes exercise delayed layout reflow.
        // 修改富文本块和视口宽度，验证延迟的布局重排。
        reply.Blocks.Clear();
        AddParagraph("# 最终内容\n" + new string('排', 3500));
        follow.ContentChanged(); await Settle(); Bottom("final rich-text reconciliation follows bottom");
        _window!.AppWindow.Resize(new Windows.Graphics.SizeInt32(490, 520));
        await Settle(); Bottom("narrower width reflow follows bottom");
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(490, 370));
        await Settle(); Bottom("viewport shrink follows bottom");

        scroll.ChangeView(null, scroll.ScrollableHeight - 250, null, disableAnimation: true);
        await Settle();
        double paused = scroll.VerticalOffset;
        AddParagraph(new string('增', 500)); follow.ContentChanged();
        await Settle(); Stable(paused, "scroll up pauses follow");
        scroll.ChangeView(null, scroll.ScrollableHeight, null, disableAnimation: true);
        await Settle();
        AddParagraph(new string('续', 500)); follow.ContentChanged();
        await Settle(); Bottom("scroll back to bottom resumes follow");

        TextSelectionAutoScroll.HasActiveSelection = true;
        paused = scroll.VerticalOffset;
        AddParagraph(new string('选', 500)); follow.ContentChanged();
        await Settle(); Stable(paused, "selection pauses follow");
        TextSelectionAutoScroll.HasActiveSelection = false;
        follow.ContentChanged(); await Settle(); Stable(paused, "selection release keeps reader position");
        follow.OpenChat(); await Settle(); Bottom("open chat restores following after selection");

        Chat(2); follow.OpenChat(); await Settle(); Bottom("long to short conversation opens at bottom");
        Chat(24); follow.OpenChat(); await Settle(); Bottom("short to long conversation opens at bottom");
        Chat(15); follow.OpenChat(); await Settle(); Bottom("long to shorter scrolling conversation opens at bottom");
        Chat(27); follow.OpenChat(); await Settle(); Bottom("long to longer conversation opens at bottom");
        scroll.ChangeView(null, 0, null, disableAnimation: true); await Settle();
        Chat(19); follow.OpenChat(); await Settle(); Bottom("conversation switch overrides previous reading pause");
        File.WriteAllText(_result, string.Join('\n', _checks) + "\nPASS: all native ListView follow, pause, selection, reflow and conversation-switch checks.");
    }

    private void Check(bool condition, string description)
    {
        if (!condition) throw new InvalidOperationException(description);
        _checks.Add("PASS: " + description);
    }

    private static T? FindChild<T>(DependencyObject node) where T : DependencyObject
    {
        for (int i = 0; i < VisualTreeHelper.GetChildrenCount(node); i++)
        {
            var child = VisualTreeHelper.GetChild(node, i);
            if (child is T match) return match;
            if (FindChild<T>(child) is { } descendant) return descendant;
        }
        return null;
    }
}
