using CommunityToolkit.Mvvm.ComponentModel;
using System.Collections.ObjectModel;

namespace KYNXA_Desktop.ViewModels;

public sealed record RecentConversation(string Title, string RelativeTime);

/// <summary>Presentation state for the initial KYNXA shell.</summary>
public partial class ShellViewModel : ObservableObject
{
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

    [ObservableProperty]
    public partial bool IsChatMode { get; set; } = true;

    [ObservableProperty]
    public partial string Prompt { get; set; } = string.Empty;
}
