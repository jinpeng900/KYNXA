using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using KYNXA_Desktop.Models.UI;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.ViewModels;

/// <summary>Data-backed tree rows, safe to recycle when projects are reordered.</summary>
public partial class ProjectTreeEntry(ProjectState project, ProjectChatState? chat = null) : ObservableObject
{
    public ProjectState Project { get; } = project;
    public ProjectChatState? Chat { get; } = chat;
    public string Title => Chat?.Title ?? Project.Name;
    public string Glyph => Project.IsPinned ? "\uE718" : "\uE8B7";
    public Visibility ProjectOnlyVisibility => Chat is null ? Visibility.Visible : Visibility.Collapsed;
    public string MoreLabel => Chat is null ? "项目操作" : "聊天操作";
    public string MoreId => Chat is null ? $"ProjectMore_{Project.Id:N}" : $"ChatMore_{Chat.Id:N}";
    public string AddChatId => $"ProjectChatAdd_{Project.Id:N}";
    public ObservableCollection<ProjectTreeEntry> Children { get; } = [];

    [ObservableProperty]
    public partial bool IsExpanded { get; set; }
}
