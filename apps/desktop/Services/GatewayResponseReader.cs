using System.Net;
using System.Net.Http.Json;
using System.Text.Json;

namespace KYNXA_Desktop.Services;

/// <summary>Reads gateway JSON and errors. The caller owns the response, including streaming responses.</summary>
internal static class GatewayResponseReader
{
    public static async Task<T> ReadAsync<T>(HttpResponseMessage response, string emptyResponseMessage,
        string httpErrorFormat, CancellationToken cancellationToken, JsonSerializerOptions? options = null,
        string? conflictMessage = null)
    {
        await EnsureSuccessAsync(response, httpErrorFormat, cancellationToken, conflictMessage);
        return await response.Content.ReadFromJsonAsync<T>(options, cancellationToken)
            ?? throw new InvalidDataException(emptyResponseMessage);
    }

    public static async Task EnsureSuccessAsync(HttpResponseMessage response, string httpErrorFormat,
        CancellationToken cancellationToken, string? conflictMessage = null)
    {
        if (response.IsSuccessStatusCode) return;
        // A catalog conflict has a specific recovery instruction and must not depend on its response body.
        if (response.StatusCode == HttpStatusCode.Conflict && conflictMessage is not null)
            throw new GatewayApiException(conflictMessage, response.StatusCode);

        string? message = null;
        string? errorCode = null;
        try
        {
            using var body = await response.Content.ReadFromJsonAsync<JsonDocument>(cancellationToken);
            if (body?.RootElement.ValueKind == JsonValueKind.Object)
            {
                if (body.RootElement.TryGetProperty("error", out var error) && error.ValueKind == JsonValueKind.String)
                    message = error.GetString();
                if (body.RootElement.TryGetProperty("code", out var code) && code.ValueKind == JsonValueKind.String)
                    errorCode = code.GetString();
            }
        }
        catch (JsonException)
        {
            // HTML/text failures are reported by status; never display an untrusted raw response body.
        }
        throw new GatewayApiException(message ?? string.Format(httpErrorFormat, (int)response.StatusCode),
            response.StatusCode, errorCode);
    }
}
