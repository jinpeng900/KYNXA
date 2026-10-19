using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Media.Animation;

namespace Kynxa.UsageMonitor.Controls;

/// <summary>
/// Retargetable decimal reels use the upstream animateAmount cubic easing with a per-digit vertical track.
/// 采用上游 animateAmount 的三次缓动，将数字更新移植为可重定向的逐位竖向滚动。
/// Adapted from DeepSeek-Balance-Whale-Widget, Copyright (c) 2026 MeteorNOX, MIT.
/// </summary>
public sealed class RollingNumber : StackPanel
{
    private string _text = "";
    private Brush _ink = new SolidColorBrush(Color.FromRgb(0x20, 0x24, 0x2C));

    public RollingNumber()
    {
        Orientation = Orientation.Horizontal;
        HorizontalAlignment = HorizontalAlignment.Right;
        VerticalAlignment = VerticalAlignment.Center;
    }

    public void SetForeground(Brush brush)
    {
        _ink = brush;
        SetText(_text, false, true);
    }

    public void SetText(string text, bool animate = true, bool force = false)
    {
        if (_text == text && !force) return;
        string previous = _text;
        _text = text;
        Children.Clear();
        // Align positions from the right so adding a thousands separator does not shift digit identity.
        // 从右侧对齐字符位置，增加千位分隔符时仍能找到对应的旧数字。
        for (int index = 0; index < text.Length; index++)
        {
            char next = text[index];
            int previousIndex = previous.Length - text.Length + index;
            char old = previousIndex >= 0 && previousIndex < previous.Length ? previous[previousIndex] : next;
            if (char.IsAsciiDigit(next))
            {
                Children.Add(CreateReel(char.IsAsciiDigit(old) ? old - '0' : next - '0', next - '0', animate));
            }
            else
            {
                Children.Add(new TextBlock
                {
                    Text = next.ToString(), FontSize = 21, FontWeight = FontWeights.SemiBold,
                    Foreground = _ink, Height = 29, VerticalAlignment = VerticalAlignment.Center,
                    Padding = new Thickness(0, 1, 0, 0),
                });
            }
        }
        AutomationProperties.SetName(this, text);
    }

    private FrameworkElement CreateReel(int oldDigit, int nextDigit, bool animate)
    {
        const double height = 29;
        Grid viewport = new() { Width = 14, Height = height, ClipToBounds = true };
        Canvas track = new() { Width = 14, Height = height * 30 };
        for (int index = 0; index < 30; index++)
        {
            TextBlock digit = new()
            {
                Text = (index % 10).ToString(), FontFamily = new FontFamily("Segoe UI"), FontSize = 22,
                FontWeight = FontWeights.Medium, Foreground = _ink, Height = height, Width = 14,
                TextAlignment = TextAlignment.Center,
            };
            Canvas.SetTop(digit, index * height);
            track.Children.Add(digit);
        }
        TranslateTransform translation = new(0, -nextDigit * height);
        track.RenderTransform = translation;
        viewport.Children.Add(track);
        if (animate && oldDigit != nextDigit && SystemParameters.ClientAreaAnimation)
        {
            int targetIndex = nextDigit > oldDigit ? nextDigit : nextDigit + 10;
            DoubleAnimation motion = new(-oldDigit * height, -targetIndex * height, TimeSpan.FromMilliseconds(350))
            {
                EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut },
                FillBehavior = FillBehavior.Stop,
            };
            translation.BeginAnimation(TranslateTransform.YProperty, motion);
        }
        return viewport;
    }
}
