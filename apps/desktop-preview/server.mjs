import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, resolve, sep } from "node:path";
import { spawn } from "node:child_process";
import process from "node:process";

const previewRoot = resolve(import.meta.dirname);
const desktopAssetsRoot = resolve(previewRoot, "../desktop/Resources/UI");
const portArgument = process.argv.find((value) => value.startsWith("--port="));
const port = Number(portArgument?.split("=")[1] ?? process.env.KYNXA_PREVIEW_PORT ?? 4173);
const modelApi = process.env.KYNXA_MODEL_API_URL ?? "http://127.0.0.1:5218";

const contentTypes = {
  ".css": "text/css; charset=utf-8",
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".png": "image/png",
  ".svg": "image/svg+xml"
};

function safeFile(root, requestPath) {
  const candidate = resolve(root, `.${requestPath}`);
  return candidate === root || candidate.startsWith(`${root}${sep}`) ? candidate : null;
}

const server = createServer(async (request, response) => {
  const url = new URL(request.url ?? "/", "http://localhost");
  if (url.pathname.startsWith("/api/")) {
    try {
      const upstream = await fetch(new URL(url.pathname, modelApi), {
        method: request.method,
        headers: { "Content-Type": "application/json" },
        body: request.method === "GET" ? undefined : request,
        duplex: request.method === "GET" ? undefined : "half",
        signal: AbortSignal.timeout(300000)
      });
      response.writeHead(upstream.status, { "Content-Type": "application/json; charset=utf-8", "Cache-Control": "no-store" });
      response.end(Buffer.from(await upstream.arrayBuffer()));
    } catch {
      response.writeHead(502, { "Content-Type": "application/json; charset=utf-8" });
      response.end(JSON.stringify({ error: "模型服务未启动。请先运行 apps/model-gateway。" }));
    }
    return;
  }
  let root = previewRoot;
  let pathname = decodeURIComponent(url.pathname);

  if (pathname.startsWith("/desktop-assets/")) {
    root = desktopAssetsRoot;
    pathname = pathname.slice("/desktop-assets".length);
  } else if (pathname === "/") {
    pathname = "/index.html";
  }

  const file = safeFile(root, pathname);
  if (!file) {
    response.writeHead(400).end("Invalid path");
    return;
  }

  try {
    const body = await readFile(file);
    response.writeHead(200, {
      "Content-Type": contentTypes[extname(file)] ?? "application/octet-stream",
      "Cache-Control": "no-cache"
    });
    response.end(body);
  } catch (error) {
    response.writeHead(error?.code === "ENOENT" ? 404 : 500).end("Not found");
  }
});

server.listen(port, "127.0.0.1", () => {
  const url = `http://127.0.0.1:${port}`;
  console.log(`KYNXA desktop preview: ${url}`);
  console.log("Press Ctrl+C to stop.");

  if (process.argv.includes("--open")) {
    const opener = process.platform === "darwin" ? "open" : "xdg-open";
    spawn(opener, [url], { detached: true, stdio: "ignore" }).unref();
  }
});
