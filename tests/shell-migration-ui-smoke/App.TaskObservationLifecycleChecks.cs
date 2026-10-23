using System.Reflection;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Views;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop;

public partial class App
{
    private Task CheckBackgroundReplyObservationAsync(ProjectChatState chat, object pending)
    {
        var recent = _shell.ViewModel.RecentConversations.Single(item => item.Id == chat.Id);
        var recoveryField = typeof(ShellPage).GetField("_runningTaskRecovery", PrivateInstance)!;
        var recoveryType = typeof(ShellPage).GetNestedType("RunningTaskRecovery", BindingFlags.NonPublic)!;
        var chats = ShellField<List<ProjectChatState>>("_standaloneChats");
        var readGate = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        using var cancellation = new CancellationTokenSource();
        ProjectChatState? selected = null;
        try
        {
            Call("NewStandaloneChat_Click", _shell, new RoutedEventArgs());
            selected = ShellField<ProjectChatState>("_activeStandaloneChat");
            selected.IsPersisted = true;
            // Hold B's observer before attachment, while A retains its original pending reply.
            // B 尚在读取活动任务，A 仍保留原回复，覆盖后台结束回调跨聊天到达的窗口。
            var recovery = Activator.CreateInstance(recoveryType,
                BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic,
                null, [selected, null, cancellation], null)!;
            recoveryType.GetProperty("ReadTask")!.SetValue(recovery, readGate.Task);
            recoveryField.SetValue(_shell, recovery);
            Check((bool)Call("HasQueuedTaskActivity", selected.Id)!,
                "the selected conversation retains its activity barrier while its task observer is reading");
            Call("ReleaseTaskReplyObservation", pending);
            Check(ReferenceEquals(recoveryField.GetValue(_shell), recovery) && !cancellation.IsCancellationRequested
                && (bool)Call("HasQueuedTaskActivity", selected.Id)!,
                "a departed background reply cannot cancel the selected conversation observer or release its queue barrier");
            Call("RecoverRunningTaskObservation", chat, null);
            Check(ReferenceEquals(recoveryField.GetValue(_shell), recovery) && !cancellation.IsCancellationRequested,
                "a stale recovery request is rejected before replacing the selected conversation observer");
        }
        finally
        {
            recoveryField.SetValue(_shell, null);
            readGate.TrySetResult();
            cancellation.Cancel();
            if (selected is not null) chats.Remove(selected);
            Call("RebuildStandaloneRows");
            Call("SelectStandaloneChat", recent);
        }
        return Task.CompletedTask;
    }
}
