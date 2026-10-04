using System.Net;

namespace KYNXA_Desktop.Services;

/// <summary>
/// A gateway failure retains its HTTP identity while remaining compatible with existing UI error handling.
/// 保留网关错误的 HTTP 身份，并兼容现有 UI 错误处理。
/// </summary>
public sealed class GatewayApiException(string message, HttpStatusCode statusCode, string? errorCode = null)
    : InvalidOperationException(message)
{
    public HttpStatusCode StatusCode { get; } = statusCode;
    public string? ErrorCode { get; } = errorCode;
}
