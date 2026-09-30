using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;

namespace MarkdownUiSmoke;

public partial class App
{
    private void RunTableProbe()
    {
        var document = new RichTextBlock { IsTextSelectionEnabled = true, FontSize = 14,
            FontFamily = new FontFamily("Microsoft YaHei UI"), TextWrapping = TextWrapping.NoWrap };
        var observations = new List<(string Kind, Run First, Inline Gap, Run Second)>();
        foreach (string kind in new[] { "container", "space", "nbsp", "tab" })
            foreach (string value in new[] { "a", "中文名称", "very long value" })
            {
                var paragraph = new Paragraph { Margin = new Thickness(0, 4, 0, 0) };
                var first = new Run { Text = value };
                var measure = new TextBlock { Text = value, FontSize = 14, FontFamily = document.FontFamily };
                measure.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
                double gapWidth = 200 - measure.DesiredSize.Width;
                Inline gap;
                if (kind == "container") gap = new InlineUIContainer { Child = new Border { Width = gapWidth, Height = 1, IsHitTestVisible = false } };
                else if (kind == "tab") gap = new Run { Text = "\t" };
                else
                {
                    string text = kind == "nbsp" ? "\u00a0" : " ";
                    measure.Text = text;
                    measure.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
                    gap = new Run { Text = text, CharacterSpacing = (int)Math.Round((gapWidth - measure.DesiredSize.Width) * 1000 / 14),
                        Foreground = new SolidColorBrush(Colors.Transparent) };
                }
                var second = new Run { Text = "│ next " + kind };
                paragraph.Inlines.Add(first); paragraph.Inlines.Add(gap); paragraph.Inlines.Add(second);
                document.Blocks.Add(paragraph);
                observations.Add((kind, first, gap, second));
            }
        _window = new Window { Title = "KYNXA table alignment probe", Content = new Grid { Padding = new Thickness(20), Children = { document } } };
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(780, 700));
        document.Loaded += async (_, _) =>
        {
            try
            {
                await Task.Delay(100);
                var adjustments = observations.Where(row => row.Kind is "space" or "nbsp")
                    .Select(row => (Run: (Run)row.Gap, Delta: (int)Math.Round((200 - row.Second.ContentStart.GetCharacterRect(LogicalDirection.Forward).X) * 1000 / document.FontSize))).ToArray();
                foreach (var adjustment in adjustments) adjustment.Run.CharacterSpacing += adjustment.Delta;
                await Task.Delay(100);
                document.SelectAll();
                string selected = document.SelectedText;
                document.Select(document.ContentStart, document.ContentStart);
                var report = new
                {
                    ParagraphProperties = typeof(Paragraph).GetProperties().Where(property => property.Name.Contains("Tab") || property.Name.Contains("Indent")).Select(property => property.Name).ToArray(),
                    SelectedText = selected,
                    rows = observations.Select(row => new
                    {
                        row.Kind, First = row.First.Text,
                        start = row.First.ContentStart.GetCharacterRect(LogicalDirection.Forward).X,
                        firstEnd = row.First.ContentEnd.GetCharacterRect(LogicalDirection.Backward).X,
                        gapStart = row.Gap.ContentStart.GetCharacterRect(LogicalDirection.Forward).X,
                        gapEnd = row.Gap.ContentEnd.GetCharacterRect(LogicalDirection.Backward).X,
                        next = row.Second.ContentStart.GetCharacterRect(LogicalDirection.Forward).X,
                    }).ToArray(),
                };
                File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-table-probe.json"), System.Text.Json.JsonSerializer.Serialize(report, new System.Text.Json.JsonSerializerOptions { WriteIndented = true }));
            }
            catch (Exception error) { File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-table-probe.json"), "FAIL: " + error); }
            finally { _window?.Close(); }
        };
        _window.Activate();
    }
}
