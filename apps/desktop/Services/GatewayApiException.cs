using System.Net;

namespace KYNXA_Desktop.Services;

/// <summary>A gateway failure retains its HTTP identity while remaining compatible with existing UI error handling.</summary>
public sealed class GatewayApiException(string message, HttpStatusCode statusCode, string? errorCode = null)
    : InvalidOperationException(message)
{
    public HttpStatusCode StatusCode { get; } = statusCode;
    public string? ErrorCode { get; } = errorCode;
}
