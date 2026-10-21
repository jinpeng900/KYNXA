using System.Numerics;
using KYNXA_Desktop.Services;
using Microsoft.UI.Composition;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Hosting;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Markup;
using Microsoft.UI.Xaml.Media;
using Windows.System;
using Windows.UI.ViewManagement;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// A compact radio group with a compositor-owned sliding capsule; labels never move with the selection.
/// 紧凑单选分段控件；合成器仅移动选中胶囊，文字不随选择移动。
/// </summary>
public sealed partial class SegmentedSelector : UserControl, IDisposable
{
    public sealed record Option(string Key, string LabelKey, string? IconGlyph = null);

    private readonly Grid _track = new();
    private readonly Grid _items = new();
    private readonly Border _surface = new() { CornerRadius = new CornerRadius(12), Padding = new Thickness(3) };
    private readonly List<(Option Option, SegmentButton Button, TextBlock Label)> _options = [];
    private readonly UISettings _systemSettings = new();
    private ShapeVisual? _capsule;
    private CompositionColorBrush? _capsuleFill;
    private CompositionColorBrush? _capsuleStroke;
    private ImplicitAnimationCollection? _animations;
    private string? _selectedKey;
    private bool _refreshing, _subscribed, _disposed, _positioned;

    public event EventHandler<string>? SelectionChanged;

    public string? SelectedKey
    {
        get => _selectedKey;
        set => Select(value, focus: false);
    }

    public SegmentedSelector(IEnumerable<Option> options)
    {
        var entries = options.ToArray();
        if (entries.Length == 0 || entries.Select(option => option.Key).Distinct(StringComparer.Ordinal).Count() != entries.Length)
            throw new ArgumentException("A segmented selector requires non-empty, unique option keys.", nameof(options));
        MinHeight = 36;
        HorizontalAlignment = HorizontalAlignment.Left;
        var group = new Grid();
        group.Children.Add(_track);
        group.Children.Add(_items);
        _surface.Child = group;
        Content = _surface;
        string groupName = "KynxaSegmented" + Guid.NewGuid().ToString("N");
        var template = (ControlTemplate)XamlReader.Load(SegmentTemplate);
        foreach (var option in entries)
        {
            _items.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            var label = new TextBlock
            {
                FontSize = 13, VerticalAlignment = VerticalAlignment.Center,
                FontFamily = (FontFamily)Application.Current.Resources["KynxaUIFont"]
            };
            var content = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
            if (option.IconGlyph is { Length: > 0 } glyph)
                content.Children.Add(new FontIcon { Glyph = glyph, FontSize = 13, VerticalAlignment = VerticalAlignment.Center });
            content.Children.Add(label);
            var button = new SegmentButton(this)
            {
                GroupName = groupName, Tag = option.Key, Content = content, Template = template,
                Padding = new Thickness(12, 6, 12, 6), MinWidth = 0, MinHeight = 30,
                HorizontalAlignment = HorizontalAlignment.Stretch, VerticalAlignment = VerticalAlignment.Stretch,
                HorizontalContentAlignment = HorizontalAlignment.Center, VerticalContentAlignment = VerticalAlignment.Center,
                UseSystemFocusVisuals = true, FocusVisualPrimaryThickness = new Thickness(1),
                FocusVisualSecondaryThickness = new Thickness(0), CornerRadius = new CornerRadius(9)
            };
            AutomationProperties.SetAutomationId(button, "Segment_" + option.Key);
            button.Checked += (_, _) => { if (!_refreshing) Select(option.Key, focus: false); };
            Grid.SetColumn(button, _options.Count);
            _options.Add((option, button, label));
            _items.Children.Add(button);
        }
        _selectedKey = entries[0].Key;
        RefreshLabels();
        RefreshSelection();
        RefreshAppearance();
        Loaded += SelectorLoaded;
        Unloaded += SelectorUnloaded;
        _items.SizeChanged += ItemsSizeChanged;
    }

    private void Select(string? key, bool focus)
    {
        if (_disposed || key is null || !_options.Any(entry => entry.Option.Key == key)) return;
        bool changed = !string.Equals(_selectedKey, key, StringComparison.Ordinal);
        // Preference refreshes echo the selected key; they must not cancel an in-flight capsule transition.
        // 偏好刷新会回写同一个选中值；不得因此取消正在进行的胶囊移动。
        if (!changed)
        {
            if (focus) _options.First(entry => entry.Option.Key == key).Button.Focus(FocusState.Keyboard);
            return;
        }
        _selectedKey = key;
        RefreshSelection();
        PositionCapsule(animate: changed);
        if (focus) _options.First(entry => entry.Option.Key == key).Button.Focus(FocusState.Keyboard);
        if (changed) SelectionChanged?.Invoke(this, key);
    }

    private void RefreshSelection()
    {
        _refreshing = true;
        try
        {
            foreach (var entry in _options)
            {
                bool selected = entry.Option.Key == _selectedKey;
                entry.Button.IsChecked = selected;
                entry.Button.IsTabStop = selected;
                entry.Button.Opacity = selected ? 1 : 0.74;
            }
        }
        finally { _refreshing = false; }
    }

    private void SelectorLoaded(object sender, RoutedEventArgs args)
    {
        if (_disposed) return;
        if (!_subscribed)
        {
            _subscribed = true;
            UiText.LanguageChanged += LanguageChanged;
            AppearanceService.Changed += AppearanceChanged;
        }
        EnsureCapsule();
        RefreshLabels();
        RefreshAppearance();
        PositionCapsule(animate: false);
    }

    private void SelectorUnloaded(object sender, RoutedEventArgs args)
    {
        Unsubscribe();
        _positioned = false;
    }

    private void EnsureCapsule()
    {
        if (_capsule is not null) return;
        var compositor = ElementCompositionPreview.GetElementVisual(_track).Compositor;
        _capsule = compositor.CreateShapeVisual();
        var geometry = compositor.CreateRoundedRectangleGeometry();
        geometry.CornerRadius = new Vector2(9);
        var size = compositor.CreateExpressionAnimation("capsule.Size");
        size.SetReferenceParameter("capsule", _capsule);
        geometry.StartAnimation("Size", size);
        var shape = compositor.CreateSpriteShape(geometry);
        _capsuleFill = compositor.CreateColorBrush();
        _capsuleStroke = compositor.CreateColorBrush();
        shape.FillBrush = _capsuleFill;
        shape.StrokeBrush = _capsuleStroke;
        shape.StrokeThickness = 1;
        _capsule.Shapes.Add(shape);
        _animations = compositor.CreateImplicitAnimationCollection();
        var offset = compositor.CreateVector3KeyFrameAnimation();
        offset.Target = "Offset";
        offset.Duration = TimeSpan.FromMilliseconds(200);
        offset.InsertExpressionKeyFrame(1, "this.FinalValue");
        var width = compositor.CreateVector2KeyFrameAnimation();
        width.Target = "Size";
        width.Duration = offset.Duration;
        width.InsertExpressionKeyFrame(1, "this.FinalValue");
        _animations["Offset"] = offset;
        _animations["Size"] = width;
        ElementCompositionPreview.SetElementChildVisual(_track, _capsule);
    }

    private void PositionCapsule(bool animate)
    {
        if (_disposed || _capsule is null || _options.FirstOrDefault(entry => entry.Option.Key == _selectedKey).Button is not { } button
            || button.ActualWidth <= 0 || button.ActualHeight <= 0) return;
        var position = button.TransformToVisual(_track).TransformPoint(default);
        var offset = new Vector3((float)position.X, (float)position.Y, 0);
        var size = new Vector2((float)button.ActualWidth, (float)button.ActualHeight);
        if (_positioned && _capsule.Offset == offset && _capsule.Size == size) return;
        // Implicit compositor animations retarget from the current presentation during rapid consecutive clicks.
        // 连续点击时隐式合成动画从当前可见位置接续，不停止旧动画后跳回起点。
        _capsule.ImplicitAnimations = animate && _positioned && _systemSettings.AnimationsEnabled ? _animations : null;
        _capsule.Offset = offset;
        _capsule.Size = size;
        // Initial layout and language resizing snap once, then restore transitions for the next user selection.
        // 初始布局和语言尺寸变化只立即定位一次，随后恢复下一次用户切换所需的动画。
        _capsule.ImplicitAnimations = _systemSettings.AnimationsEnabled ? _animations : null;
        _positioned = true;
    }

    private void ItemsSizeChanged(object sender, SizeChangedEventArgs args) => PositionCapsule(animate: false);

    private void RefreshLabels()
    {
        foreach (var entry in _options)
        {
            entry.Label.Text = UiText.Get(entry.Option.LabelKey);
            AutomationProperties.SetName(entry.Button, entry.Label.Text);
        }
    }

    private void RefreshAppearance()
    {
        _surface.Background = AppearanceService.GetBrush("KynxaSettingsCardBrush");
        foreach (var entry in _options)
        {
            entry.Button.Foreground = AppearanceService.GetBrush("KynxaTextBrush");
            entry.Label.Foreground = entry.Button.Foreground;
            entry.Button.FocusVisualPrimaryBrush = AppearanceService.GetBrush("KynxaSecondaryTextBrush");
        }
        if (_capsuleFill is not null) _capsuleFill.Color = AppearanceService.GetBrush("KynxaSelectionBrush").Color;
        if (_capsuleStroke is not null) _capsuleStroke.Color = AppearanceService.GetBrush("KynxaDividerBrush").Color;
    }

    private void LanguageChanged(object? sender, EventArgs args) => DispatcherQueue.TryEnqueue(() =>
    {
        if (_disposed) return;
        RefreshLabels();
        DispatcherQueue.TryEnqueue(() => PositionCapsule(animate: false));
    });

    private void AppearanceChanged(object? sender, EventArgs args) => DispatcherQueue.TryEnqueue(() =>
    {
        if (!_disposed) RefreshAppearance();
    });

    private bool HandleNavigation(VirtualKey key)
    {
        int index = _options.FindIndex(entry => entry.Option.Key == _selectedKey);
        int direction = FlowDirection == FlowDirection.RightToLeft ? -1 : 1;
        int next = key switch
        {
            VirtualKey.Left => (index - direction + _options.Count) % _options.Count,
            VirtualKey.Right => (index + direction + _options.Count) % _options.Count,
            VirtualKey.Home => 0,
            VirtualKey.End => _options.Count - 1,
            _ => -1
        };
        if (next < 0) return false;
        Select(_options[next].Option.Key, focus: true);
        return true;
    }

    private void Unsubscribe()
    {
        if (!_subscribed) return;
        _subscribed = false;
        UiText.LanguageChanged -= LanguageChanged;
        AppearanceService.Changed -= AppearanceChanged;
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        Unsubscribe();
        Loaded -= SelectorLoaded;
        Unloaded -= SelectorUnloaded;
        _items.SizeChanged -= ItemsSizeChanged;
        ElementCompositionPreview.SetElementChildVisual(_track, null);
        _capsule?.Dispose();
        _capsuleFill?.Dispose();
        _capsuleStroke?.Dispose();
        _animations?.Dispose();
    }

    private sealed partial class SegmentButton(SegmentedSelector owner) : RadioButton
    {
        protected override void OnKeyDown(KeyRoutedEventArgs args)
        {
            if (owner.HandleNavigation(args.Key)) args.Handled = true;
            else base.OnKeyDown(args);
        }
    }

    private const string SegmentTemplate = """
        <ControlTemplate xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" TargetType="RadioButton">
          <Border x:Name="Surface" xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml" Background="Transparent" CornerRadius="9" Padding="{TemplateBinding Padding}">
            <VisualStateManager.VisualStateGroups>
              <VisualStateGroup x:Name="CommonStates">
                <VisualState x:Name="Normal"/>
                <VisualState x:Name="PointerOver"><VisualState.Setters><Setter Target="Surface.Background" Value="{ThemeResource KynxaControlHoverBrush}"/></VisualState.Setters></VisualState>
                <VisualState x:Name="Pressed"><VisualState.Setters><Setter Target="Surface.Background" Value="{ThemeResource KynxaControlPressedBrush}"/></VisualState.Setters></VisualState>
                <VisualState x:Name="Disabled"><VisualState.Setters><Setter Target="Surface.Opacity" Value="0.45"/></VisualState.Setters></VisualState>
              </VisualStateGroup>
            </VisualStateManager.VisualStateGroups>
            <ContentPresenter Content="{TemplateBinding Content}" HorizontalAlignment="Center" VerticalAlignment="Center"/>
          </Border>
        </ControlTemplate>
        """;
}
