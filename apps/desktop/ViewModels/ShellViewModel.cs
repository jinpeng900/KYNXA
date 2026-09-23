using CommunityToolkit.Mvvm.ComponentModel;
using System.Collections.ObjectModel;

namespace KYNXA_Desktop.ViewModels;

public sealed record RecentConversation(string Title, string RelativeTime)
{
    public Guid Id { get; init; } = Guid.NewGuid();
    public string MoreId => $"ChatMore_{Id:N}";
}

public sealed record SidebarProject(string Name, IReadOnlyList<string> Conversations);

/// <summary>Presentation state for the initial KYNXA shell.</summary>
public partial class ShellViewModel : ObservableObject
{
    // Sample projects for the UI prototype; no persisted user history is implied.
    public IReadOnlyList<SidebarProject> Projects { get; } =
    [
        new("KYNXA 界面设计", ["侧栏布局与导航", "工作与聊天切换", "输入框交互细节", "浅色主题与字体"]),
        new("毕业论文", ["研究问题与提纲", "文献阅读与归纳", "数据清洗与分析", "图表与结果讨论"]),
        new("CodeRepair 开发", ["定位构建错误", "修复方案讨论", "补充回归验证", "整理发布说明"]),
        new("市场推广计划", ["目标用户分析", "内容选题规划", "活动页面文案", "渠道效果复盘"]),
        new("个人知识库", ["整理阅读笔记", "知识分类与标签", "构建检索索引", "每周学习回顾"]),
        new("数据分析练习", ["探索性数据分析", "回归模型比较", "可视化方案", "分析报告初稿"]),
        new("本地模型评测", ["设计测试任务", "推理速度记录", "回答质量对比", "评测结果总结"]),
        new("旅行准备", ["目的地与路线", "行程安排", "预算与物品清单", "整理出行笔记"])
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
        new("AIGC 应用案例分析", "1 个月前"),
        new("周末阅读书单", "1 个月前"),
        new("统计学概念梳理", "1 个月前"),
        new("英语写作润色", "1 个月前"),
        new("面试准备与练习", "1 个月前"),
        new("整理会议纪要", "1 个月前"),
        new("Python 自动化入门", "1 个月前"),
        new("演示文稿结构建议", "1 个月前"),
        new("每周时间安排", "1 个月前"),
        new("摄影构图小技巧", "1 个月前"),
        new("认识大语言模型", "1 个月前")
    ];

    [ObservableProperty]
    public partial bool IsChatMode { get; set; } = false;

    [ObservableProperty]
    public partial string Prompt { get; set; } = string.Empty;

}
