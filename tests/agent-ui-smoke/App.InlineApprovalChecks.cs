using KYNXA.Contracts;
using KYNXA_Desktop.Controls;
using MemoryUiSmoke;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace AgentUiSmoke;

public partial class App
{
    private async Task CheckInlineApprovalCardAsync()
    {
        var activity = new ToolActivity("inline-fixture", "terminal.host.run", null, "approval-required",
            "Write the isolated fixture document", ApprovalId: Guid.NewGuid(), OutsideWorkspace: false,
            WorkspaceRoot: Path.Combine(_directory, "Workspace"), TaskApprovalAvailable: true,
            TaskApprovalDescription: "本任务内允许相同工具、参数与工作范围；更换目标或配置需再次批准。暂停后保留，重启后需重新批准。");
        var root = (Grid)_root;
        var overlay = new Grid();
        Grid.SetRowSpan(overlay, Math.Max(1, root.RowDefinitions.Count));
        Grid.SetColumnSpan(overlay, Math.Max(1, root.ColumnDefinitions.Count));
        Canvas.SetZIndex(overlay, 100);
        var card = new InlineToolApprovalCard(activity)
        {
            Width = 320, HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Top
        };
        var decisions = new List<ToolApprovalDecision>();
        card.DecisionSelected += decisions.Add;
        overlay.Children.Add(card);
        root.Children.Add(overlay);
        try
        {
            await WaitAsync(() => card.IsLoaded && card.ActualWidth > 0, "inline approval loads inside the existing window");
            var once = NativeUi.ByName<Button>(card, "InlineToolApprovalOnce");
            var task = NativeUi.ByName<Button>(card, "InlineToolApprovalTask");
            var reject = NativeUi.ByName<Button>(card, "InlineToolApprovalReject");
            NativeUi.ByName<Expander>(card, "InlineToolApprovalDetails").IsExpanded = true;
            card.SetAvailableHeight(440);
            await SettleAsync();
            var origin = new Windows.Foundation.Point();
            var contentScroll = NativeUi.ByName<ScrollViewer>(card, "InlineToolApprovalContentScroll");
            double detailsHeight = contentScroll.MaxHeight;
            Check(decisions.Count == 0 && task.Visibility == Visibility.Visible && NativeUi.OpenDialog(_root) is null &&
                reject.TransformToVisual(card).TransformPoint(origin).Y > once.TransformToVisual(card).TransformPoint(origin).Y + 4 &&
                new[] { once, task, reject }.All(button => button.ActualWidth > 0 &&
                    button.TransformToVisual(card).TransformPoint(origin).X + button.ActualWidth <= card.ActualWidth + 1 &&
                    button.TransformToVisual(card).TransformPoint(origin).Y + button.ActualHeight <= card.ActualHeight + 1) &&
                card.ActualHeight <= 241 && contentScroll.ScrollableHeight > 0 &&
                double.IsFinite(detailsHeight) && detailsHeight is > 0 and < 440 && double.IsInfinity(card.MaxHeight),
                "short narrow inline approval keeps both action rows inside the card and reserves input space while complete operation text scrolls");
            Check(NativeUi.ByName<TextBlock>(card, "InlineToolApprovalExecutionBoundary") is { IsTextTrimmed: false } boundary &&
                boundary.Text == KYNXA_Desktop.Services.UiText.Get("本机终端在沙箱外执行，可能修改电脑文件和设置。"),
                "host execution boundary remains complete in the readable operation area");
            NativeUi.Invoke(task);
            Check(decisions.Count == 1 && decisions[0] == new ToolApprovalDecision(true, "task") &&
                !once.IsEnabled && !task.IsEnabled && !reject.IsEnabled,
                "an explicit task approval emits one structured decision and disables every competing action");

            overlay.Children.Clear();
            const string sensitiveReadNotice = "读取可能含密钥的文件，批准后内容可能进入工具记录并发送给当前模型。";
            var onceOnly = new InlineToolApprovalCard(activity with
                { Name = "filesystem.read", Summary = sensitiveReadNotice, TaskApprovalDescription = null })
            {
                Width = 320, HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Top
            };
            var denied = new List<ToolApprovalDecision>();
            onceOnly.DecisionSelected += denied.Add;
            overlay.Children.Add(onceOnly);
            await WaitAsync(() => onceOnly.IsLoaded && onceOnly.ActualWidth > 0, "unscoped approval loads without a task grant");
            onceOnly.SetAvailableHeight(440);
            await SettleAsync();
            Check(NativeUi.ByName<Button>(onceOnly, "InlineToolApprovalTask").Visibility == Visibility.Collapsed &&
                NativeUi.ByName<TextBlock>(onceOnly, "InlineToolApprovalTaskScope").Visibility == Visibility.Collapsed && denied.Count == 0,
                "missing verified scope description cannot expose task authorization");
            Check(NativeUi.ByName<TextBlock>(onceOnly, "InlineToolApprovalSummary") is { IsTextTrimmed: false } summary &&
                summary.Text == KYNXA_Desktop.Services.UiText.Get(sensitiveReadNotice),
                "sensitive reading notice retains its full localized model-disclosure text");
            NativeUi.Invoke(NativeUi.ByName<Button>(onceOnly, "InlineToolApprovalReject"));
            Check(denied.Count == 1 && denied[0] == new ToolApprovalDecision(false), "inline refusal remains an explicit single-call denial");
        }
        finally { root.Children.Remove(overlay); }
    }
}
