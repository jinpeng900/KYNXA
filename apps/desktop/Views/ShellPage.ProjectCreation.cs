using KYNXA_Desktop.Controls;
using KYNXA_Desktop.Models.UI;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using Windows.Storage;
using Windows.Storage.Pickers;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private sealed class ProjectCreationDraft
    {
        public Guid ProjectId { get; } = Guid.NewGuid();
        public TaskCompletionSource<ProjectState?> Completion { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public List<ProjectMountState> Mounts { get; } = [];
        public Guid? DefaultMountId { get; set; }
        public bool Picking { get; set; }
        public bool Committing { get; set; }
    }
    private ProjectCreationDraft? _projectCreation;

    private Task<ProjectState?> ShowProjectCreationAsync(StorageFolder? initialFolder = null)
    {
        if (_projectCreation is { } existing) return existing.Completion.Task;
        if (_projectViewClosed || _chatClosing || _savingOnClose || XamlRoot is null)
            return Task.FromResult<ProjectState?>(null);
        var draft = _projectCreation = new ProjectCreationDraft();
        Guid? originalChat = ActiveChatId, originalProject = _selectedWorkProjectId;
        bool originalMode = ViewModel.IsChatMode;
        var root = XamlRoot;
        var flyout = PickerMenu.Create(FlyoutPlacementMode.BottomEdgeAlignedLeft);
        var content = new StackPanel { Spacing = 12 };
        content.Width = PickerMenu.SetContentWidth(flyout, 420, XamlRoot.Size.Width);
        var name = new SubmitTextBox { Name = "ProjectCreationName", PlaceholderText = UiText.Get("项目名称"),
            MaxLength = 80, MinWidth = 0, CornerRadius = new CornerRadius(8) };
        var neutral = AppearanceService.GetBrush("KynxaSecondaryTextBrush");
        AutomationProperties.SetAutomationId(name, "ProjectCreationName");
        AutomationProperties.SetName(name, UiText.Get("项目名称"));
        name.FocusVisualPrimaryBrush = neutral;
        name.FocusVisualSecondaryBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        name.Resources["TextControlBorderBrushFocused"] = neutral;
        name.Resources["TextControlBorderBrushPointerOver"] = neutral;
        var heading = new Grid();
        heading.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        heading.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        heading.Children.Add(new TextBlock { Text = UiText.Get("创建项目"), FontSize = 18, VerticalAlignment = VerticalAlignment.Center });
        var close = PickerMenu.Action(UiText.Get("关闭"), "ProjectCreationClose", "\uE711");
        close.Content = new FontIcon { Glyph = "\uE711", FontSize = 12 };
        close.Click += (_, _) => flyout.Hide();
        Grid.SetColumn(close, 1); heading.Children.Add(close); content.Children.Add(heading);
        content.Children.Add(name);
        content.Children.Add(new TextBlock { Text = UiText.Get("源文件夹（可选，可添加多个）"), FontSize = 12, Foreground = neutral });
        var folders = new StackPanel { Spacing = 5 };
        AutomationProperties.SetAutomationId(folders, "ProjectCreationFolderRows");
        var folderScroll = new ScrollViewer { Content = folders, Height = 0, Visibility = Visibility.Collapsed,
            Style = (Style)Application.Current.Resources["KynxaFormScrollViewerStyle"] };
        AutomationProperties.SetAutomationId(folderScroll, "ProjectCreationFolders");
        content.Children.Add(folderScroll);
        var add = PickerMenu.Action(UiText.Get("添加文件夹"), "ProjectCreationAddFolder", "\uE8F4");
        var create = PickerMenu.Action(UiText.Get("创建"), "ProjectCreationCommit");
        var cancel = PickerMenu.Action(UiText.Get("取消"), "ProjectCreationCancel");
        cancel.Click += (_, _) => flyout.Hide();
        var footer = new Grid { ColumnSpacing = 12 };
        footer.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        footer.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        footer.Children.Add(new TextBlock { Text = UiText.Get("Enter 创建 · Esc 取消"), FontSize = 11,
            Foreground = neutral, VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis });
        var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        actions.Children.Add(cancel); actions.Children.Add(create);
        Grid.SetColumn(actions, 1); footer.Children.Add(actions);
        content.Children.Add(add); content.Children.Add(footer); flyout.Content = content;
        bool Current() => !_projectViewClosed && !_chatClosing && !_savingOnClose && ReferenceEquals(_projectCreation, draft) &&
            ActiveChatId == originalChat && _selectedWorkProjectId == originalProject && ViewModel.IsChatMode == originalMode;
        void Cancel()
        {
            if (ReferenceEquals(_projectCreation, draft)) _projectCreation = null;
            draft.Completion.TrySetResult(null);
        }
        Task HideForOperationAsync()
        {
            var closed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            void Closed(object? sender, object args)
            {
                flyout.Closed -= Closed;
                closed.TrySetResult();
            }
            // Keep operation flags until Closed; a fast failure must not reopen while the old dismissal can cancel it.
            // 保留操作状态直到 Closed；快速失败时不能在旧关闭回调仍可取消草稿的阶段重新打开。
            flyout.Closed += Closed;
            flyout.Hide();
            return closed.Task;
        }
        void UpdateFolderListHeight()
        {
            if (folders.Children.Count == 0)
            {
                folderScroll.Visibility = Visibility.Collapsed;
                folderScroll.Height = 0;
                return;
            }
            folderScroll.Visibility = Visibility.Visible;
            var fixedChildren = content.Children.OfType<FrameworkElement>()
                .Where(child => child != folderScroll && child.Visibility == Visibility.Visible).ToArray();
            var measureSize = new Windows.Foundation.Size(content.Width, double.PositiveInfinity);
            foreach (var child in fixedChildren) child.Measure(measureSize);
            double fixedHeight = fixedChildren.Sum(child => child.DesiredSize.Height) + content.Spacing * fixedChildren.Length;
            double budget = Math.Max(0, PickerMenu.AvailableContentHeight(root.Size.Height) - fixedHeight);
            double chrome = folderScroll.Padding.Top + folderScroll.Padding.Bottom +
                folderScroll.BorderThickness.Top + folderScroll.BorderThickness.Bottom;
            double rowWidth = Math.Max(0, content.Width - folderScroll.Padding.Left - folderScroll.Padding.Right -
                folderScroll.BorderThickness.Left - folderScroll.BorderThickness.Right);
            double height = chrome;
            int visible = 0;
            // Size only the visible prefix; native scrolling retains every other folder and its actions.
            // 只按完整可见的前几项限高；其他文件夹及其操作继续保留在原生滚动区内。
            foreach (var row in folders.Children.OfType<Grid>().Take(PickerMenu.VisibleRows))
            {
                foreach (var control in row.Children.OfType<Control>()) control.ApplyTemplate();
                row.Measure(new Windows.Foundation.Size(rowWidth, double.PositiveInfinity));
                double nextHeight = height + (visible > 0 ? folders.Spacing : 0) + row.DesiredSize.Height;
                if (visible > 0 && nextHeight > budget) break;
                height = nextHeight;
                visible++;
            }
            folderScroll.Height = height;
        }
        void RootChanged(XamlRoot sender, XamlRootChangedEventArgs args) => UpdateFolderListHeight();
        void RenderFolders()
        {
            folders.Children.Clear();
            foreach (var mount in draft.Mounts)
            {
                var row = new Grid { ColumnSpacing = 6, Padding = new Thickness(3) };
                AutomationProperties.SetAutomationId(row, $"ProjectCreationFolder_{mount.Id:N}");
                row.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
                row.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
                row.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
                var selected = new RadioButton { IsChecked = draft.DefaultMountId == mount.Id,
                    Content = UiText.Get("默认"), MinWidth = 0, VerticalAlignment = VerticalAlignment.Center };
                AutomationProperties.SetAutomationId(selected, $"ProjectCreationDefault_{mount.Id:N}");
                AutomationProperties.SetName(selected, UiText.Get("默认") + ": " + mount.Name);
                foreach (string key in new[] { "RadioButtonOuterEllipseCheckedFill", "RadioButtonOuterEllipseCheckedStroke",
                    "RadioButtonOuterEllipseCheckedPointerOverFill", "RadioButtonOuterEllipseCheckedPressedFill",
                    "RadioButtonOuterEllipseCheckedPointerOverStroke", "RadioButtonOuterEllipseCheckedPressedStroke",
                    "RadioButtonCheckGlyphFill", "RadioButtonCheckGlyphFillPointerOver", "RadioButtonCheckGlyphFillPressed" }) selected.Resources[key] = neutral;
                selected.FocusVisualPrimaryBrush = neutral;
                selected.FocusVisualSecondaryBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
                selected.Checked += (_, _) =>
                {
                    if (draft.DefaultMountId == mount.Id) return;
                    draft.DefaultMountId = mount.Id; RenderFolders();
                };
                var label = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
                label.Children.Add(new TextBlock { Text = mount.Name, FontSize = 12, TextTrimming = TextTrimming.CharacterEllipsis });
                label.Children.Add(new TextBlock { Text = mount.Path, FontSize = 11, Foreground = neutral,
                    TextTrimming = TextTrimming.CharacterEllipsis });
                ToolTipService.SetToolTip(label, mount.Path);
                var remove = PickerMenu.Action(UiText.Get("移除"), $"ProjectCreationRemoveFolder_{mount.Id:N}", "\uE711");
                remove.Click += (_, _) =>
                {
                    draft.Mounts.Remove(mount);
                    if (draft.DefaultMountId == mount.Id) draft.DefaultMountId = draft.Mounts.FirstOrDefault()?.Id;
                    RenderFolders();
                };
                row.Children.Add(selected); Grid.SetColumn(label, 1); row.Children.Add(label);
                Grid.SetColumn(remove, 2); row.Children.Add(remove); folders.Children.Add(row);
            }
            UpdateFolderListHeight();
        }
        void AddFolder(StorageFolder folder)
        {
            string path = Path.TrimEndingDirectorySeparator(folder.Path);
            if (draft.Mounts.Any(mount => string.Equals(Path.TrimEndingDirectorySeparator(mount.Path), path, StringComparison.OrdinalIgnoreCase))) return;
            string alias = folder.DisplayName;
            for (int number = 2; draft.Mounts.Any(mount => string.Equals(mount.Name, alias, StringComparison.OrdinalIgnoreCase)); number++)
                alias = $"{folder.DisplayName} ({number})";
            var mount = new ProjectMountState(Guid.NewGuid(), alias, path);
            draft.Mounts.Add(mount); draft.DefaultMountId ??= mount.Id;
            if (string.IsNullOrWhiteSpace(name.Text)) name.Text = folder.DisplayName;
            RenderFolders();
        }
        if (initialFolder is not null) AddFolder(initialFolder);
        else UpdateFolderListHeight();
        add.Click += async (_, _) =>
        {
            if (!Current() || draft.Picking) return;
            // The native picker temporarily hides this light-dismiss popup; that transition must retain its draft.
            // 原生目录选择器临时隐藏轻弹层；失焦关闭不代表用户丢弃名称和已选目录。
            draft.Picking = true;
            Task closing = HideForOperationAsync();
            try
            {
                var picker = new FolderPicker { SuggestedStartLocation = PickerLocationId.DocumentsLibrary };
                picker.FileTypeFilter.Add("*");
                WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(App.Window));
                var folder = await picker.PickSingleFolderAsync();
                if (Current() && folder is not null) AddFolder(folder);
            }
            catch (Exception error) when (error is System.Runtime.InteropServices.COMException or UnauthorizedAccessException or IOException)
            { if (Current()) ShowActionFeedback(error.Message); }
            finally
            {
                await closing;
                draft.Picking = false;
                if (Current()) flyout.ShowAt(AddProjectButton); else Cancel();
            }
        };
        async Task CommitAsync()
        {
            if (!Current() || draft.Picking || draft.Committing || _projectActionPending) return;
            draft.Committing = true;
            Task closing = HideForOperationAsync();
            ProjectState? created = null;
            await RunProjectActionAsync(async () =>
            {
                var project = new ProjectState { Id = draft.ProjectId, Name = string.IsNullOrWhiteSpace(name.Text) ? UiText.Get("新建项目") : name.Text.Trim(),
                    Mounts = draft.Mounts.ToList(), DefaultMountId = draft.DefaultMountId,
                    FolderPath = draft.Mounts.FirstOrDefault(mount => mount.Id == draft.DefaultMountId)?.Path };
                _projects.Insert(0, project);
                try
                {
                    await SaveProjectsAndRenderAsync();
                    created = project;
                }
                catch (Exception error) when (error is HttpRequestException or OperationCanceledException or InvalidOperationException or
                    InvalidDataException or System.Text.Json.JsonException)
                {
                    // Failed catalog submission keeps the same draft identity; no directory import starts before acceptance.
                    // 目录提交失败保留同一草稿身份；正式接收前不开始资料导入，用户可直接修正重试。
                    _projects.Remove(project); RenderProjects(); ShowActionFeedback(error.Message);
                }
            });
            await closing;
            if (created is null)
            {
                draft.Committing = false;
                if (Current()) flyout.ShowAt(AddProjectButton); else Cancel();
                return;
            }
            if (ReferenceEquals(_projectCreation, draft)) _projectCreation = null;
            draft.Completion.TrySetResult(created);
        }
        create.Click += async (_, _) => await CommitAsync();
        name.SubmitRequested += async (_, _) => await CommitAsync();
        flyout.Closed += (_, _) =>
        {
            root.Changed -= RootChanged;
            if (!draft.Picking && !draft.Committing) Cancel();
        };
        flyout.Opened += (_, _) =>
        {
            root.Changed += RootChanged;
            UpdateFolderListHeight();
            name.Focus(FocusState.Programmatic);
        };
        flyout.ShowAt(AddProjectButton);
        return draft.Completion.Task;
    }

}
