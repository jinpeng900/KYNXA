using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using KYNXA_Desktop.Models.UI;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.ViewModels;

/// <summary>
/// Data-backed tree rows, safe to recycle when projects are reordered.
/// 由正式数据支持的树行；项目重排时可安全复用。
/// </summary>
public partial class ProjectTreeEntry(ProjectState project, ProjectChatState? chat = null) : ObservableObject
{
    private string _title = chat?.Title ?? project.Name;
    private string _glyph = (chat?.IsPinned ?? project.IsPinned) ? "\uE718" : "\uE8B7";
    private bool _isPinned = chat?.IsPinned ?? project.IsPinned;
    private string? _mountedFolder;
    public ProjectState Project { get; private set; } = project;
    public ProjectChatState? Chat { get; private set; } = chat;
    public string Title => _title;
    public override string ToString() => Title;
    public string ContextTitle => Chat is null || Project.IsFolderlessWorkspace ? Title : $"{Project.Name} / {Title}";
    public string? MountedFolderTooltip => _mountedFolder;
    public string MountedFolderName
    {
        get
        {
            if (_mountedFolder is null) return "";
            if (Services.ProjectMountPresentation.WorkspaceMount(Project) is { } mount) return mount.Name;
            string name = Path.GetFileName(Path.TrimEndingDirectorySeparator(_mountedFolder));
            return name.Length == 0 ? _mountedFolder : name;
        }
    }
    public Visibility MountedFolderVisibility => _mountedFolder is null ? Visibility.Collapsed : Visibility.Visible;

    public void SetMountedFolderPath(string? path)
    {
        path = Chat is null && !Project.IsFolderlessWorkspace ? path : null;
        SetProperty(ref _mountedFolder, path, nameof(MountedFolderTooltip));
        OnPropertyChanged(nameof(MountedFolderTooltip));
        OnPropertyChanged(nameof(MountedFolderName));
        OnPropertyChanged(nameof(MountedFolderVisibility));
    }
    public string Glyph => _glyph;
    public bool IsPinned => _isPinned;
    public Visibility PinnedVisibility => IsPinned ? Visibility.Visible : Visibility.Collapsed;
    public Visibility UnpinnedProjectVisibility => Chat is null && !IsPinned ? Visibility.Visible : Visibility.Collapsed;
    public string PinStatusKey => IsPinned ? "已置顶" : string.Empty;
    public string PinIconKey => Chat is null ? "IconPinnedProject" : "IconPinnedRecent";
    public Visibility ProjectOnlyVisibility => Chat is null ? Visibility.Visible : Visibility.Collapsed;
    public string MoreLabel => Chat is null ? Services.UiText.Get("项目操作") : Services.UiText.Get("聊天操作");
    public string MoreId => Chat is null ? $"ProjectMore_{Project.Id:N}" : $"ChatMore_{Chat.Id:N}";
    public string AddChatId => $"ProjectChatAdd_{Project.Id:N}";
    public ObservableCollection<ProjectTreeEntry> Children { get; } = [];
    public void RefreshLanguage()
    {
        OnPropertyChanged(nameof(MoreLabel));
        foreach (var child in Children) child.RefreshLanguage();
    }
    public Visibility ActiveVisibility => IsActive ? Visibility.Visible : Visibility.Collapsed;
    public Visibility ReplyingVisibility => IsReplying ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>
    /// Refresh metadata without replacing the row or its native TreeView container.
    /// 刷新元数据时保留原行及其原生 TreeView 容器。
    /// </summary>
    public void Refresh(ProjectState project, ProjectChatState? chat = null)
    {
        if (Project.Id != project.Id || Chat?.Id != chat?.Id)
            throw new ArgumentException("A project tree row must keep its project and chat identity.");
        if (!ReferenceEquals(Project, project))
        {
            Project = project;
            OnPropertyChanged(nameof(Project));
        }
        if (!ReferenceEquals(Chat, chat))
        {
            Chat = chat;
            OnPropertyChanged(nameof(Chat));
        }
        SetProperty(ref _title, chat?.Title ?? project.Name, nameof(Title));
        OnPropertyChanged(nameof(ContextTitle));
        OnPropertyChanged(nameof(MountedFolderTooltip));
        SetProperty(ref _glyph, (chat?.IsPinned ?? project.IsPinned) ? "\uE718" : "\uE8B7", nameof(Glyph));
        // Pinning mutates the same model instance; cache its presented value so recycled rows notify bindings.
        // 置顶会修改同一模型实例，缓存展示值，确保复用行仍能通知绑定刷新。
        if (SetProperty(ref _isPinned, chat?.IsPinned ?? project.IsPinned, nameof(IsPinned)))
        {
            OnPropertyChanged(nameof(PinnedVisibility));
            OnPropertyChanged(nameof(UnpinnedProjectVisibility));
            OnPropertyChanged(nameof(PinStatusKey));
        }
    }

    [ObservableProperty]
    public partial bool IsExpanded { get; set; }

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ActiveVisibility))]
    public partial bool IsActive { get; set; }

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ReplyingVisibility))]
    public partial bool IsReplying { get; set; }
}
