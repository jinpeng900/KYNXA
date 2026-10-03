using System.Text;
using KYNXA_Desktop.Models.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;
using Windows.Storage.Pickers;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private string _workDetailTab = "tasks";
    private string _workResultText = string.Empty;
    private bool _featureInfoOpen;

    private void DismissSidebar_Click(object sender, RoutedEventArgs e)
    {
        _compactSidebarOpen = false;
        ApplyLayout();
    }

    private void UpdateModelStatusPresentation()
    {
        if (ModelStatusText is null) return;
        bool error = _modelLoadError is not null;
        string status = _modelsLoading ? "正在读取模型连接…"
            : error ? "无法连接模型服务，请重试。"
            : !_modelsLoaded ? "正在准备模型服务…"
            : _availableModels.Length == 0 ? "先配置一个模型连接，再开始对话。"
            : _selectedModel is null ? "模型连接已加载，请选择要使用的模型。"
            : $"当前模型：{_selectedModel.Name} · {_selectedModel.ProviderName}";
        ModelStatusText.Text = status;
        ModelRetryButton.Visibility = error ? Visibility.Visible : Visibility.Collapsed;
        ModelRetryButton.IsEnabled = !_modelsLoading;
        ModelSetupButton.Content = _availableModels.Length > 0 && _selectedModel is null ? "选择模型" : "配置模型";
        ModelSetupButton.IsEnabled = !_modelsLoading;
        ModelStatusBar.Visibility = ActiveMessages.Count > 0 && !error && _selectedModel is not null && !_modelsLoading
            ? Visibility.Collapsed : Visibility.Visible;
        GatewayStatusDot.Fill = (Brush)Application.Current.Resources[
            _modelsLoaded && !error && !_modelsLoading ? "KynxaOnlineBrush" : "KynxaSecondaryTextBrush"];
        string serviceStatus = _modelsLoading ? "正在读取模型连接"
            : error ? "模型服务连接失败" : _modelsLoaded ? "模型网关可用；模型是否可调用需在模型管理中测试" : "正在准备模型服务";
        ToolTipService.SetToolTip(GatewayStatusHost, serviceStatus);
        AutomationProperties.SetName(GatewayStatusHost, serviceStatus);
        ToolTipService.SetToolTip(ModelStatusText, error ? _modelLoadError : status);
        UpdateWorkDetailPresentation();
        ApplyLayout();
    }

    private async void ModelRetry_Click(object sender, RoutedEventArgs e) => await RefreshModelPickerAsync();

    private void ModelSetup_Click(object sender, RoutedEventArgs e)
    {
        if (_availableModels.Length > 0 && _selectedModel is null) ModelPickerButton_Click(ModelPickerButton, e);
        else OpenModelManagement();
    }

    private void StartChat_Click(object sender, RoutedEventArgs e)
    {
        SetPrimaryMode(true);
        if (_activeStandaloneChat is null) NewStandaloneChat_Click(sender, e);
        PromptTextBox.Focus(FocusState.Programmatic);
    }

    private void StartWork_Click(object sender, RoutedEventArgs e)
    {
        SetPrimaryMode(false);
        if (_activeProjectChat is null) ShowWorkspacePicker();
        else { _layout.PreviewVisible = true; ApplyLayout(); }
    }

    private void ExamplePrompt_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: string example }) return;
        PromptTextBox.Text = string.IsNullOrWhiteSpace(PromptTextBox.Text)
            ? example + Environment.NewLine : PromptTextBox.Text + Environment.NewLine + example + Environment.NewLine;
        PromptTextBox.SelectionStart = PromptTextBox.Text.Length;
        PromptTextBox.Focus(FocusState.Programmatic);
    }

    private async void PlannedModule_Click(object sender, RoutedEventArgs e)
    {
        string name = (sender as FrameworkElement)?.Tag as string ?? "此功能";
        string description = name switch
        {
            "知识库" => "未来可以在这里整理资料、搜索内容并在对话中引用。目前尚未开放资料导入和检索。",
            "连接" => "未来可以在这里管理应用和工具连接。目前模型连接请在“模型管理”中配置。",
            "定时任务" => "未来可以在这里安排定期执行的工作。目前尚未开放自动执行。",
            "技能" => "未来可以在这里管理可复用的任务方法。目前尚未开放技能加载和执行。",
            _ => "此功能仍在规划中。"
        };
        await ShowFeatureInfoAsync(name + " · 规划中", description);
    }

    private async void AttachmentInfo_Click(object sender, RoutedEventArgs e) =>
        await ShowFeatureInfoAsync("附件 · 规划中", "目前支持文本对话。你可以把需要解释的文字或代码粘贴到输入框；文件和图片上传尚未开放。");

    private async Task ShowFeatureInfoAsync(string title, string description)
    {
        if (_featureInfoOpen || XamlRoot is null) return;
        _featureInfoOpen = true;
        try
        {
            await new ContentDialog
            {
                XamlRoot = XamlRoot, Title = title,
                Content = new TextBlock { Text = description, TextWrapping = TextWrapping.Wrap, MaxWidth = 420 },
                CloseButtonText = "知道了", DefaultButton = ContentDialogButton.Close
            }.ShowAsync();
        }
        finally { _featureInfoOpen = false; }
    }

    private void WorkDetailTab_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string tab }) _workDetailTab = tab;
        UpdateWorkDetailPresentation();
    }

    private void UpdateWorkDetailPresentation()
    {
        if (WorkDetailTitle is null) return;
        var chat = _activeProjectChat;
        var project = chat is null ? null : _projects.FirstOrDefault(p => p.Chats.Contains(chat));
        WorkDetailTitle.Text = project?.Name ?? "当前工作";
        WorkDetailSubtitle.Text = chat?.Title ?? "选择一个工作，查看它的详情。";
        WorkTasksContent.Visibility = _workDetailTab == "tasks" ? Visibility.Visible : Visibility.Collapsed;
        WorkFilesContent.Visibility = _workDetailTab == "files" ? Visibility.Visible : Visibility.Collapsed;
        WorkResultsContent.Visibility = _workDetailTab == "results" ? Visibility.Visible : Visibility.Collapsed;
        foreach (var tab in new[] { WorkTasksTab, WorkFilesTab, WorkResultsTab })
        {
            bool selected = tab.Tag as string == _workDetailTab;
            tab.Background = selected ? (Brush)Application.Current.Resources["KynxaSegmentIdleBrush"]
                : new SolidColorBrush(Microsoft.UI.Colors.Transparent);
            AutomationProperties.SetHelpText(tab, selected ? "已选中" : "未选中");
        }
        _pendingReplies.TryGetValue(chat?.Id ?? Guid.Empty, out var pending);
        WorkRequestStatus.Text = pending is not null
            ? pending.Error is null ? "正在等待模型回复" : "本次请求未完成"
            : chat?.Messages.LastOrDefault()?.Role == "assistant" ? "回复已保存"
            : chat?.Messages.Count > 0 ? "需求已保存" : "等待输入需求";
        int replies = chat?.Messages.Count(m => m.Role == "assistant") ?? 0;
        WorkRequestSummary.Text = chat is null ? "从一条具体需求开始。"
            : $"{chat.Messages.Count} 条消息 · {replies} 条模型回复" +
              (pending?.Error is string error ? Environment.NewLine + error : string.Empty);
        WorkFolderPath.Text = project?.FolderPath ?? "此工作没有关联文件夹。";
        CopyWorkPathButton.IsEnabled = !string.IsNullOrWhiteSpace(project?.FolderPath);
        _workResultText = chat?.Messages.LastOrDefault(m => m.Role == "assistant")?.Content ?? string.Empty;
        WorkResultStatus.Text = replies == 0 ? "还没有模型回复。结果会在这里显示。" : $"已保存 {replies} 条回复，下面是最近一条。";
        WorkResultPreview.Text = _workResultText.Length > 1200 ? _workResultText[..1200] + "…" : _workResultText;
        CopyWorkResultButton.IsEnabled = _workResultText.Length > 0;
        SynchronizeWorkFilesContext();
    }

    private bool SetClipboardText(string text)
    {
        try
        {
            var package = new DataPackage();
            package.SetText(text);
            Clipboard.SetContent(package);
            return true;
        }
        catch (System.Runtime.InteropServices.COMException)
        {
            _ = ShowFeatureInfoAsync("暂时无法复制", "剪贴板暂时不可用，请稍后重试，或选中文字后复制。");
            return false;
        }
    }

    private void CopyMessage_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string text } && text.Length > 0)
        {
            if (SetClipboardText(text)) ToolTipService.SetToolTip((Button)sender, "已复制");
        }
    }

    private void CopyWorkPath_Click(object sender, RoutedEventArgs e)
    {
        var project = _projects.FirstOrDefault(p => p.Chats.Contains(_activeProjectChat!));
        if (!string.IsNullOrWhiteSpace(project?.FolderPath)) SetClipboardText(project.FolderPath);
    }

    private void CopyWorkResult_Click(object sender, RoutedEventArgs e)
    {
        if (_workResultText.Length > 0) SetClipboardText(_workResultText);
    }

    private ProjectChatState? CurrentChat => ViewModel.IsChatMode ? _activeStandaloneChat : _activeProjectChat;

    private string ConversationMarkdown()
    {
        var chat = CurrentChat;
        if (chat is null) return string.Empty;
        var builder = new StringBuilder().Append("# ").AppendLine(chat.Title).AppendLine();
        foreach (var message in chat.Messages)
        {
            builder.Append("## ").AppendLine(message.Role == "user" ? "用户" : "KYNXA")
                .AppendLine().AppendLine(message.Content).AppendLine();
        }
        return builder.ToString();
    }

    private void ConversationActions_Click(object sender, RoutedEventArgs e)
    {
        var menu = new MenuFlyout();
        var copy = new MenuFlyoutItem { Text = "复制整个会话", IsEnabled = CurrentChat?.Messages.Count > 0 };
        copy.Click += (_, _) => SetClipboardText(ConversationMarkdown());
        var export = new MenuFlyoutItem { Text = "导出为 Markdown", IsEnabled = CurrentChat?.Messages.Count > 0 };
        export.Click += ExportConversation_Click;
        menu.Items.Add(copy);
        menu.Items.Add(export);
        menu.ShowAt((FrameworkElement)sender);
    }

    private async void ExportConversation_Click(object sender, RoutedEventArgs e)
    {
        var chat = CurrentChat;
        string content = ConversationMarkdown();
        if (chat is null || chat.Messages.Count == 0) return;
        try
        {
            var picker = new FileSavePicker { SuggestedStartLocation = PickerLocationId.DocumentsLibrary };
            picker.FileTypeChoices.Add("Markdown", new List<string> { ".md" });
            picker.SuggestedFileName = string.Concat(chat.Title.Select(c => Path.GetInvalidFileNameChars().Contains(c) ? '_' : c));
            WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(App.Window));
            var file = await picker.PickSaveFileAsync();
            if (file is not null) await FileIO.WriteTextAsync(file, content);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or System.Runtime.InteropServices.COMException)
        {
            await ShowFeatureInfoAsync("无法导出会话", "请确认文件位置可写，然后重试。" + Environment.NewLine + error.Message);
        }
    }

    public void ShowNavigationMenu(FrameworkElement anchor)
    {
        var menu = new MenuFlyout();
        void Add(string label, Action action)
        {
            var item = new MenuFlyoutItem { Text = label };
            item.Click += (_, _) => action();
            menu.Items.Add(item);
        }
        Add("开始聊天", () => StartChat_Click(anchor, new RoutedEventArgs()));
        Add("选择工作", () => StartWork_Click(anchor, new RoutedEventArgs()));
        Add("模型管理", () => OpenModelManagement());
        Add("数据与存储", () => StorageSettings_Click(anchor, new RoutedEventArgs()));
        Add("键盘操作", () => ShortcutHelp_Click(anchor, new RoutedEventArgs()));
        menu.Items.Add(new MenuFlyoutSeparator());
        Add(RecentArea.Visibility == Visibility.Visible ? "收起侧栏" : "展开侧栏", () => SidebarToggle_Click(anchor, new RoutedEventArgs()));
        menu.ShowAt(anchor);
    }
}
