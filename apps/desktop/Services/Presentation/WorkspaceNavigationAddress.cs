using System.Net;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Both workspace address bars navigate URLs and send ordinary text to the same search provider.
/// 工作区两处地址输入共用解释规则：网址直接打开，普通文字交给同一搜索来源。
/// </summary>
public static class WorkspaceNavigationAddress
{
    public static bool TryResolve(string? input, out Uri address)
    {
        address = null!;
        string value = input?.Trim() ?? "";
        if (value.Length == 0) return false;
        if (Uri.TryCreate(value, UriKind.Absolute, out var absolute) && absolute.Scheme is "http" or "https")
        {
            address = absolute;
            return true;
        }
        if (value.Contains("://", StringComparison.Ordinal)) return false;
        if (!value.Any(char.IsWhiteSpace) && Uri.TryCreate("https://" + value.TrimStart('/'), UriKind.Absolute, out var website)
            && (website.Host.Contains('.') || website.Host.Equals("localhost", StringComparison.OrdinalIgnoreCase)
                || IPAddress.TryParse(website.Host, out _)))
        {
            address = website;
            return true;
        }
        address = new Uri("https://www.google.com/search?q=" + Uri.EscapeDataString(value));
        return true;
    }
}
