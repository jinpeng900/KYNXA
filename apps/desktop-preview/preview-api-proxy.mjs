import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";

/**
 * Owns transport and cancellation only; the model gateway remains the API owner.
 * 仅负责传输与取消；模型网关仍拥有 API 的业务和数据职责。
 */
export async function proxyPreviewApi(request, response, url, modelApi) {
  const requestCancellation = new AbortController();
  const abortUpstreamRequest = () => requestCancellation.abort();
  request.once("aborted", abortUpstreamRequest);
  response.once("close", abortUpstreamRequest);
  try {
    const sendsBody = request.method !== "GET" && request.method !== "HEAD";
    const upstreamResponse = await fetch(new URL(`${url.pathname}${url.search}`, modelApi), {
      method: request.method,
      headers: { "Content-Type": request.headers["content-type"] ?? "application/json" },
      body: sendsBody ? request : undefined,
      duplex: sendsBody ? "half" : undefined,
      signal: AbortSignal.any([requestCancellation.signal, AbortSignal.timeout(300000)])
    });
    response.writeHead(upstreamResponse.status, {
      "Content-Type": upstreamResponse.headers.get("content-type") ?? "application/json; charset=utf-8",
      "Cache-Control": "no-store"
    });
    if (upstreamResponse.body) await pipeline(Readable.fromWeb(upstreamResponse.body), response);
    else response.end();
  } catch {
    if (response.destroyed || requestCancellation.signal.aborted) return;
    if (response.headersSent) {
      response.destroy();
      return;
    }
    response.writeHead(502, { "Content-Type": "application/json; charset=utf-8" });
    response.end(JSON.stringify({ error: "模型服务未启动。请先运行 apps/model-gateway。" }));
  } finally {
    request.off("aborted", abortUpstreamRequest);
    response.off("close", abortUpstreamRequest);
  }
}
