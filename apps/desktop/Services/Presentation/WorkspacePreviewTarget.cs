namespace KYNXA_Desktop.Services;

/// <summary>
/// An authorized gateway projection, not a second file or artifact store.
/// 网关授权后的预览投影，不建立第二份文件或产物存储。
/// </summary>
public sealed record WorkspacePreviewTarget
{
    public string Kind { get; init; } = "text";
    public string Title { get; init; } = "";
    public string Path { get; init; } = "";
    public string Version { get; init; } = "";
    public string? Language { get; init; }
    public string MimeType { get; init; } = "text/plain";
    public string? Text { get; init; }
    public string[][]? Rows { get; init; }
    public string? Delimiter { get; init; }
    public string? ContentUrl { get; init; }
    public bool HasMore { get; init; }
    public int? NextOffset { get; init; }
    public string? Notice { get; init; }
}
