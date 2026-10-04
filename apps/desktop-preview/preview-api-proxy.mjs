import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";

/** Owns transport and cancellation only; the model gateway remains the API owner. */
export async function proxyPreviewApi(request, response, url, modelApi) {
  const cancellation = new AbortController();
  const abort = () => cancellation.abort();
  request.once("aborted", abort);
  response.once("close", abort);
  try {
    const sendsBody = request.method !== "GET" && request.method !== "HEAD";
    const upstream = await fetch(new URL(`${url.pathname}${url.search}`, modelApi), {
      method: request.method,
      headers: { "Content-Type": request.headers["content-type"] ?? "application/json" },
      body: sendsBody ? request : undefined,
      duplex: sendsBody ? "half" : undefined,
      signal: AbortSignal.any([cancellation.signal, AbortSignal.timeout(300000)])
    });
    response.writeHead(upstream.status, {
      "Content-Type": upstream.headers.get("content-type") ?? "application/json; charset=utf-8",
      "Cache-Control": "no-store"
    });
    if (upstream.body) await pipeline(Readable.fromWeb(upstream.body), response);
    else response.end();
  } catch {
    if (response.destroyed || cancellation.signal.aborted) return;
    if (response.headersSent) {
      response.destroy();
      return;
    }
    response.writeHead(502, { "Content-Type": "application/json; charset=utf-8" });
    response.end(JSON.stringify({ error: "模型服务未启动。请先运行 apps/model-gateway。" }));
  } finally {
    request.off("aborted", abort);
    response.off("close", abort);
  }
}
