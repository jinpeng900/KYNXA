using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Selects a verified paused identity after restart without inventing a default among multiple tasks.
/// 重启后从已校验的暂停任务选择唯一身份；多项任务不能默认猜选。
/// </summary>
internal sealed record PausedTaskSelection(Guid? TaskId, bool IsAmbiguous)
{
    internal static PausedTaskSelection From(TaskPausedResponse response) => response.Tasks.Length switch
    {
        0 => new(null, false),
        1 => new(response.Tasks[0].TaskId, false),
        _ => new(null, true)
    };
}
