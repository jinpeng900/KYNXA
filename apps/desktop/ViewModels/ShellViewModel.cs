using CommunityToolkit.Mvvm.ComponentModel;
using System.Collections.ObjectModel;

namespace KYNXA_Desktop.ViewModels;

public sealed record RecentConversation(string Title, string RelativeTime);

public enum WorkCategory
{
    Personal,
    Shared
}

public sealed record WorkSummary(Guid Id, string Name, string CreatedAt, WorkCategory Category);

/// <summary>Presentation state for the initial KYNXA shell.</summary>
public partial class ShellViewModel : ObservableObject
{
    private readonly IReadOnlyList<WorkSummary> _allWorks =
    [
        new(Guid.NewGuid(), "KYNXA 桌面端界面设计", "2026-09-06", WorkCategory.Personal),
        new(Guid.NewGuid(), "CodeRepair 垂直切片", "2026-09-04", WorkCategory.Shared),
        new(Guid.NewGuid(), "毕业论文数据分析", "2026-09-03", WorkCategory.Personal),
        new(Guid.NewGuid(), "市场推广方案", "2026-08-28", WorkCategory.Shared),
        new(Guid.NewGuid(), "本地模型性能测试", "2026-08-22", WorkCategory.Personal),
        new(Guid.NewGuid(), "个人学习计划", "2026-08-16", WorkCategory.Personal),
        new(Guid.NewGuid(), "团队产品需求整理", "2026-08-09", WorkCategory.Shared),
        new(Guid.NewGuid(), "Agent 安全架构研究", "2026-07-30", WorkCategory.Shared)
    ];

    public ObservableCollection<RecentConversation> RecentConversations { get; } =
    [
        new("市场推广策略讨论", "2 小时前"),
        new("量子计算原理解释", "4 小时前"),
        new("产品命名方案", "1 天前"),
        new("总结研究论文", "2 天前"),
        new("设计落地页文案", "3 天前"),
        new("TypeScript 学习路线", "3 天前"),
        new("图像分割模型对比", "5 天前"),
        new("毕业论文选题建议", "6 天前"),
        new("深度学习训练技巧", "1 周前"),
        new("Linux 常用命令整理", "1 周前"),
        new("数据库期末复习", "1 周前"),
        new("SQL 语句优化", "1 周前"),
        new("个人成长计划", "2 周前"),
        new("AIGC 应用案例分析", "1 个月前")
    ];

    public ObservableCollection<WorkSummary> VisibleWorks { get; } = [];

    public ShellViewModel()
    {
        FilterWorks(null);
    }

    public void FilterWorks(WorkCategory? category)
    {
        VisibleWorks.Clear();
        foreach (WorkSummary work in _allWorks.Where(work => category is null || work.Category == category))
        {
            VisibleWorks.Add(work);
        }
    }

    [ObservableProperty]
    public partial bool IsChatMode { get; set; } = true;

    [ObservableProperty]
    public partial string Prompt { get; set; } = string.Empty;

    [ObservableProperty]
    public partial WorkSummary? SelectedWork { get; set; }
}
