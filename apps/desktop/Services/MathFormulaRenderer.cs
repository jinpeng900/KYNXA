using CSharpMath.SkiaSharp;
using SkiaSharp;

namespace KYNXA_Desktop.Services;

public sealed record FormulaImage(byte[] Png, double Width, double Height, double Baseline);

public static class MathFormulaRenderer
{
    public static FormulaImage? Render(string latex, bool block)
    {
        if (string.IsNullOrWhiteSpace(latex) || latex.Length > 2048) return null;
        int depth = 0;
        foreach (char c in latex)
        {
            if (c == '{' && ++depth > 32) return null;
            if (c == '}') depth--;
        }
        try
        {
            var painter = new MathPainter
            {
                LaTeX = latex, FontSize = block ? 28 : 24,
                LineStyle = block ? CSharpMath.Atom.LineStyle.Display : CSharpMath.Atom.LineStyle.Text,
                TextColor = new SKColor(35, 35, 35), DisplayErrorInline = false
            };
            if (painter.ErrorMessage is not null) return null;
            var size = painter.Measure();
            if (!float.IsFinite(size.Width) || !float.IsFinite(size.Height)
                || size.Width <= 0 || size.Height <= 0 || size.Width > 4096 || size.Height > 1024) return null;
            // Explicit padding avoids clipping ascenders/descenders at fractional pixel bounds.
            int width = (int)Math.Ceiling(size.Width) + 8, height = (int)Math.Ceiling(size.Height) + 8;
            using var surface = SKSurface.Create(new SKImageInfo(width, height));
            surface.Canvas.Clear(SKColors.Transparent);
            painter.Draw(surface.Canvas, 4, 4 - size.Y);
            using var image = surface.Snapshot();
            using var data = image.Encode(SKEncodedImageFormat.Png, 100);
            return new FormulaImage(data.ToArray(), width / 2d, height / 2d, (4 - size.Y) / 2d);
        }
        catch (Exception error) when (error is ArgumentException or InvalidOperationException or NotSupportedException)
        {
            return null; // Unsupported/incomplete LaTeX remains readable source.
        }
    }
}
