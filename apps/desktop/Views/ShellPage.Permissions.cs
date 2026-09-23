using KYNXA.Contracts;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using Windows.System;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private sealed record PermissionOption(string Mode, string Label, string Description, string Icon);

    private static readonly PermissionOption[] PermissionOptions =
    [
        new(ChatPermissionModes.Ask, "请求批准", "编辑外部文件和使用互联网时始终询问", "IconPermissionAsk"),
        new(ChatPermissionModes.Smart, "帮我批准", "仅对检测到的风险操作请求批准", "IconPermissionSmart"),
        new(ChatPermissionModes.Full, "完全访问权限", "可不受限制地访问互联网和你电脑上的任何文件", "IconPermissionFull")
    ];

    private void InitializePermissionPicker()
    {
        if (!ChatPermissionModes.IsSupported(_layout.PermissionMode)) _layout.PermissionMode = ChatPermissionModes.Ask;
        UpdatePermissionPickerLabel();
    }

    private void UpdatePermissionPickerLabel()
    {
        var option = PermissionOptions.First(option => option.Mode == _layout.PermissionMode);
        SelectedPermissionLabel.Text = option.Label;
        SelectedPermissionLabel.Foreground = PermissionBrush(option.Mode);
        SelectedPermissionIcon.Source = (ImageSource)Application.Current.Resources[option.Icon];
        AutomationProperties.SetName(PermissionPickerButton, $"权限：{option.Label}");
        ToolTipService.SetToolTip(PermissionPickerButton, option.Description);
    }

    private static Brush PermissionBrush(string mode) => (Brush)Application.Current.Resources[
        mode == ChatPermissionModes.Full ? "KynxaPermissionFullBrush" : "KynxaSecondaryTextBrush"];

    private void PermissionPickerButton_Click(object sender, RoutedEventArgs e)
    {
        var menu = new Flyout
        {
            Placement = FlyoutPlacementMode.TopEdgeAlignedLeft,
            FlyoutPresenterStyle = (Style)Application.Current.Resources["KynxaPermissionFlyoutPresenterStyle"]
        };
        var options = new StackPanel();
        var buttons = new List<Button>();
        foreach (var option in PermissionOptions)
        {
            bool selected = option.Mode == _layout.PermissionMode;
            var row = new Grid { ColumnSpacing = 10 };
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(18) });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(14) });
            row.Children.Add(new Image
            {
                Width = 18, Height = 18, VerticalAlignment = VerticalAlignment.Center,
                Source = (ImageSource)Application.Current.Resources[option.Icon]
            });
            var text = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
            text.Children.Add(new TextBlock
            {
                Text = option.Label, FontSize = 14, LineHeight = 20,
                Foreground = option.Mode == ChatPermissionModes.Full ? PermissionBrush(option.Mode)
                    : (Brush)Application.Current.Resources["KynxaTextBrush"]
            });
            text.Children.Add(new TextBlock
            {
                Text = option.Description, FontSize = 12, LineHeight = 18,
                TextWrapping = TextWrapping.Wrap, Foreground = PermissionBrush(option.Mode)
            });
            Grid.SetColumn(text, 1);
            row.Children.Add(text);
            var check = new FontIcon
            {
                Glyph = "\uE73E", FontSize = 13, Foreground = PermissionBrush(option.Mode),
                Visibility = selected ? Visibility.Visible : Visibility.Collapsed
            };
            Grid.SetColumn(check, 2);
            row.Children.Add(check);
            var button = new Button
            {
                Content = row, Style = (Style)Application.Current.Resources["KynxaPermissionOptionStyle"]
            };
            AutomationProperties.SetAutomationId(button, $"PermissionOption_{option.Mode}");
            AutomationProperties.SetName(button, option.Label);
            AutomationProperties.SetHelpText(button, $"{(selected ? "已选中。" : "")}{option.Description}");
            button.Click += (_, _) =>
            {
                _layout.PermissionMode = option.Mode;
                UpdatePermissionPickerLabel();
                ApplyLayout();
                SaveLayout();
                menu.Hide();
            };
            button.KeyDown += (_, args) =>
            {
                if (args.Key is not (VirtualKey.Up or VirtualKey.Down)) return;
                int step = args.Key == VirtualKey.Down ? 1 : -1;
                buttons[(buttons.IndexOf(button) + step + buttons.Count) % buttons.Count].Focus(FocusState.Keyboard);
                args.Handled = true;
            };
            buttons.Add(button);
            options.Children.Add(button);
        }
        menu.Content = options;
        menu.Opened += (_, _) => buttons[Array.FindIndex(PermissionOptions, option => option.Mode == _layout.PermissionMode)]
            .Focus(FocusState.Programmatic);
        menu.ShowAt(PermissionPickerButton);
    }
}
