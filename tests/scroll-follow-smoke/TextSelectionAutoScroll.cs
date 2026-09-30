namespace KYNXA_Desktop.Controls;

// The real selection control is tested separately; this fixture toggles its public state boundary.
internal static class TextSelectionAutoScroll
{
    internal static bool HasActiveSelection { get; set; }
}
