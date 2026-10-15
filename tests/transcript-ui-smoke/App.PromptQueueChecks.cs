using System.Diagnostics;
using System.Text.Json;
using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Automation.Provider;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace TranscriptUiSmoke;

public partial class App
{
    private void LaunchPromptQueueChecks(Stopwatch cold)
    {
        foreach (string name in new[] { "Colors", "Dimensions", "Controls" })
            Resources.MergedDictionaries.Add(new ResourceDictionary { Source = new Uri($"ms-appx:///Styles/{name}.xaml") });
        var queue = new ConversationPromptQueue { VerticalAlignment = VerticalAlignment.Bottom, Margin = new Thickness(16) };
        _window = new Window { Title = "KYNXA queued messages", Content = queue };
        _window.AppWindow.Resize(new Windows.Graphics.SizeInt32(720, 360));
        queue.Loaded += (_, _) => _ = RunPromptQueueChecksAsync(queue, cold);
        _window.AppWindow.Show(activateWindow: false);
    }

    private async Task RunPromptQueueChecksAsync(ConversationPromptQueue queue, Stopwatch cold)
    {
        File.WriteAllText(_result, "RUNNING: native prompt queue checks");
        try
        {
            var first = new QueuedPrompt(Guid.NewGuid(), "分析文件并保留已经取得的证据", "api", "model-a", "full");
            var second = new QueuedPrompt(Guid.NewGuid(), "重点比较成本与实施条件", "api", "model-a", "full");
            var third = new QueuedPrompt(Guid.NewGuid(), "最后整理成表格", "api", "model-a", "full");
            var pending = new List<QueuedPrompt> { first, second, third };
            Guid? steered = null, removed = null;
            var moves = new List<int>();
            queue.SteerRequested += (_, message) => { steered = message.Id; pending.Remove(message); queue.ShowMessages(pending, true); };
            queue.RemoveRequested += (_, message) => { removed = message.Id; pending.Remove(message); queue.ShowMessages(pending, true); };
            queue.MoveRequested += (_, movement) =>
            {
                moves.Add(movement.Offset);
                int index = pending.IndexOf(movement.Prompt);
                pending.RemoveAt(index);
                pending.Insert(index + movement.Offset, movement.Prompt);
                queue.ShowMessages(pending, true);
            };
            queue.ShowMessages(pending, true);
            await Task.Delay(60);
            queue.UpdateLayout();
            var rows = QueueRows(queue);
            Check(rows.Length == 3 && queue.Visibility == Visibility.Visible && rows.All(row => row.ActualHeight > 0), "three queued messages render in the native window");
            Check(((ScrollViewer)queue.Content).MaxHeight == 170, "queue scroll area stays bounded above the composer");
            Check(QueuePreview(rows[0]).Text == first.Text && QueuePreview(rows[0]).MaxLines == 2, "message preview preserves text with two-line layout");
            var retainedRow = rows[0];
            queue.ShowMessages(pending, true);
            Check(ReferenceEquals(retainedRow, QueueRows(queue)[0]), "refresh reuses existing native row controls");
            var firstActions = QueueButtons(retainedRow);
            var neutral = Resources["KynxaSecondaryTextBrush"] as Brush;
            Check(ReferenceEquals(firstActions[0].Style, Resources["KynxaQuietButtonStyle"])
                && ReferenceEquals(firstActions[1].Style, Resources["KynxaCompactIconButtonStyle"]), "queue actions use production button styles");
            Check(ReferenceEquals(firstActions[0].FocusVisualPrimaryBrush, neutral)
                && ReferenceEquals(queue.Resources["TextControlBorderBrushFocused"], neutral)
                && retainedRow.CornerRadius.TopLeft > 0, "focused actions and editing use neutral borders with rounded rows");

            InvokeQueueControl(QueueButtons(rows[1])[0]);
            await Task.Delay(35);
            Check(steered == second.Id && pending.SequenceEqual(new[] { first, third }) && QueueRows(queue).Length == 2, "guidance sends its own pending message and removes it from the displayed queue");
            Check(ReferenceEquals(retainedRow, QueueRows(queue)[0]), "guidance retains remaining message controls");

            var more = QueueButtons(retainedRow)[2];
            InvokeQueueControl(more);
            await Task.Delay(50);
            var menu = (MenuFlyout)more.Flyout;
            InvokeQueueControl(menu.Items[0]);
            await Task.Delay(70);
            var editor = QueueEditor(retainedRow);
            Check(editor.Visibility == Visibility.Visible, "edit opens an inline native editor without a dialog");
            editor.Text = "补充成本分析，并接续已有回复";
            await Task.Delay(25);
            Check(first.Text == editor.Text, "inline editing updates the original queued message");
            queue.ShowMessages(pending, true);
            Check(ReferenceEquals(editor, QueueEditor(QueueRows(queue)[0])) && editor.Text == first.Text, "queue refresh preserves the active editor and its content");

            var thirdMore = QueueButtons(QueueRows(queue)[1])[2];
            InvokeQueueControl(thirdMore);
            await Task.Delay(50);
            var thirdMenu = (MenuFlyout)thirdMore.Flyout;
            InvokeQueueControl(thirdMenu.Items[1]);
            await Task.Delay(35);
            Check(pending[0] == third && moves.SequenceEqual(new[] { -1 }), "up action changes pending message order through the public event");
            InvokeQueueControl(QueueButtons(QueueRows(queue)[0])[2]);
            await Task.Delay(50);
            InvokeQueueControl(thirdMenu.Items[2]);
            await Task.Delay(35);
            Check(pending[0] == first && moves.SequenceEqual(new[] { -1, 1 }), "down action restores the requested order");

            InvokeQueueControl(QueueButtons(QueueRows(queue)[1])[1]);
            await Task.Delay(35);
            Check(removed == third.Id && pending.SequenceEqual(new[] { first }), "delete removes only its selected queued message");
            first.IsSubmitting = true;
            pending.Add(second);
            queue.ShowMessages(pending, true);
            var submittingActions = QueueButtons(QueueRows(queue)[0]);
            Check(submittingActions.All(button => !button.IsEnabled) && QueueEditor(QueueRows(queue)[0]).IsReadOnly,
                "a submitting message disables guidance deletion movement and editing");
            queue.ShowMessages(pending, false);
            Check(!QueueButtons(QueueRows(queue)[1])[0].IsEnabled, "guidance disables when no task can accept it");
            queue.ShowMessages(Array.Empty<QueuedPrompt>(), false);
            Check(queue.Visibility == Visibility.Collapsed && QueueRows(queue).Length == 0, "empty queue hides and releases its rows");
            _metrics["checks"] = _checks;
            _metrics["mode"] = "prompt-queue-only";
            _metrics["durationMs"] = cold.ElapsedMilliseconds;
            File.WriteAllText(Path.Combine(Path.GetTempPath(), "kynxa-transcript-smoke.json"), JsonSerializer.Serialize(_metrics, new JsonSerializerOptions { WriteIndented = true }));
            File.WriteAllText(_result, $"PASS: {_checks} native queued-message checks; multi-message rendering, steering, inline edits, order, deletion, submission gating, neutral focus and retained row identity.");
        }
        catch (Exception error)
        {
            File.WriteAllText(_result, "FAIL: " + error);
        }
        finally { _window?.Close(); }
    }

    private static Border[] QueueRows(ConversationPromptQueue queue) => ((StackPanel)((ScrollViewer)queue.Content).Content).Children.OfType<Border>().ToArray();
    private static Grid QueueTextHost(Border row) => ((Grid)row.Child).Children.OfType<Grid>().Single();
    private static TextBlock QueuePreview(Border row) => QueueTextHost(row).Children.OfType<TextBlock>().Single();
    private static TextBox QueueEditor(Border row) => QueueTextHost(row).Children.OfType<TextBox>().Single();
    private static Button[] QueueButtons(Border row) => ((Grid)row.Child).Children.OfType<StackPanel>().Single().Children.OfType<Button>().ToArray();
    private static void InvokeQueueControl(FrameworkElement control)
    {
        var peer = FrameworkElementAutomationPeer.CreatePeerForElement(control)
            ?? throw new InvalidOperationException("Native queue action has no automation peer.");
        if (peer.GetPattern(PatternInterface.Invoke) is not IInvokeProvider invoke)
            throw new InvalidOperationException("Native queue action has no invocation pattern.");
        invoke.Invoke();
    }
}
