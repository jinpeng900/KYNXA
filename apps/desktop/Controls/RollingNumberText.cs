using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Animation;
using Windows.Foundation;
using Windows.UI.ViewManagement;
using FontWeights = Microsoft.UI.Text.FontWeights;

namespace KYNXA_Desktop.Controls;

/// <summary>
/// Animates only changed digits; usage values and unknown markers remain the owner's text.
/// 仅滚动变化的数字；用量及未知标记始终保留宿主提供的原文。
/// </summary>
internal sealed class RollingNumberText : UserControl
{
    private readonly StackPanel _characters = new() { Orientation = Orientation.Horizontal, Spacing = 0 };
    private readonly UISettings _systemSettings = new();
    private readonly List<DigitCell> _cells = [];
    private string _text = "";
    private Brush? _foreground;
    private FontFamily? _font;

    public RollingNumberText()
    {
        Content = _characters;
        Height = 21;
        IsHitTestVisible = false;
        VerticalAlignment = VerticalAlignment.Center;
        Unloaded += (_, _) => StopAnimations();
        AutomationProperties.SetAccessibilityView(this, Microsoft.UI.Xaml.Automation.Peers.AccessibilityView.Raw);
    }

    public void SetAppearance(FontFamily font, Brush foreground)
    {
        _font = font;
        _foreground = foreground;
        foreach (var cell in _cells) cell.SetAppearance(font, foreground);
    }

    public void Show(string text, bool animate = true)
    {
        text ??= "—";
        if (_text == text) return;
        string previous = _text;
        _text = text;
        bool canAnimate = animate && IsLoaded && _systemSettings.AnimationsEnabled;

        // Stable digit cells keep ordinary counter updates out of the composer's layout.
        // 固定数字单元避免普通计数变化触发输入框布局；仅位数变化时重建单元。
        if (_cells.Count != text.Length)
        {
            StopAnimations();
            _cells.Clear();
            _characters.Children.Clear();
            for (int index = 0; index < text.Length; index++)
            {
                var cell = new DigitCell();
                if (_font is not null && _foreground is not null) cell.SetAppearance(_font, _foreground);
                _cells.Add(cell);
                _characters.Children.Add(cell);
                int previousIndex = index - (text.Length - previous.Length);
                char old = previousIndex >= 0 && previousIndex < previous.Length ? previous[previousIndex] : '\0';
                cell.Show(old, text[index], canAnimate);
            }
            return;
        }

        for (int index = 0; index < text.Length; index++)
            _cells[index].Show(previous[index], text[index], canAnimate);
    }

    public void StopAnimations()
    {
        foreach (var cell in _cells) cell.Stop();
    }

    private sealed class DigitCell : Grid
    {
        private const double CellHeight = 21;
        private readonly TextBlock _outgoing = CreateText();
        private readonly TextBlock _incoming = CreateText();
        private readonly TranslateTransform _oldPosition = new();
        private readonly TranslateTransform _newPosition = new();
        private Storyboard? _animation;
        private char _character;

        public DigitCell()
        {
            Height = CellHeight;
            _outgoing.RenderTransform = _oldPosition;
            _incoming.RenderTransform = _newPosition;
            Children.Add(_outgoing);
            Children.Add(_incoming);
        }

        private static TextBlock CreateText() => new()
        {
            FontSize = 12.5,
            FontWeight = FontWeights.Medium,
            TextAlignment = TextAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            VerticalAlignment = VerticalAlignment.Center,
            OpticalMarginAlignment = OpticalMarginAlignment.None
        };

        public void SetAppearance(FontFamily font, Brush foreground)
        {
            _incoming.FontFamily = _outgoing.FontFamily = font;
            _incoming.Foreground = _outgoing.Foreground = foreground;
        }

        public void Show(char old, char current, bool animate)
        {
            if (_character == current) return;
            Stop();
            _character = current;
            Width = current switch
            {
                >= '0' and <= '9' => 7.6,
                ',' or '.' => 3.8,
                '%' => 11,
                '—' => 12.5,
                ' ' => 3.8,
                _ => 8.5
            };
            Clip = new RectangleGeometry { Rect = new Rect(0, 0, Width, CellHeight) };
            _incoming.Text = current.ToString();
            _outgoing.Text = "";
            _oldPosition.Y = _newPosition.Y = 0;
            if (!animate || old == current || old is < '0' or > '9' || current is < '0' or > '9') return;

            _outgoing.Text = old.ToString();
            _oldPosition.Y = 0;
            _newPosition.Y = CellHeight;
            var animation = new Storyboard();
            AddAnimation(animation, _oldPosition, 0, -CellHeight);
            AddAnimation(animation, _newPosition, CellHeight, 0);
            animation.Completed += (_, _) =>
            {
                if (!ReferenceEquals(_animation, animation)) return;
                animation.Stop();
                _oldPosition.Y = _newPosition.Y = 0;
                _outgoing.Text = "";
                _animation = null;
            };
            _animation = animation;
            animation.Begin();
        }

        private static void AddAnimation(Storyboard storyboard, TranslateTransform target, double from, double to)
        {
            var animation = new DoubleAnimation
            {
                From = from,
                To = to,
                Duration = new Duration(TimeSpan.FromMilliseconds(300)),
                EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut }
            };
            Storyboard.SetTarget(animation, target);
            Storyboard.SetTargetProperty(animation, "Y");
            storyboard.Children.Add(animation);
        }

        public void Stop()
        {
            _animation?.Stop();
            _animation = null;
            _outgoing.Text = "";
            _oldPosition.Y = _newPosition.Y = 0;
        }
    }
}
