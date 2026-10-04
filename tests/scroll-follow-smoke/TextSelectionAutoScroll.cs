namespace KYNXA_Desktop.Controls;

// The real selection control is tested separately; this fixture toggles its public state boundary.
// 真实选择控件单独测试；此夹具只切换其公开状态边界。
internal static class TextSelectionAutoScroll
{
    internal static bool HasActiveSelection { get; set; }
}
