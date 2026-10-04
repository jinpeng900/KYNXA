import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, resolve, sep } from "node:path";
import { spawn } from "node:child_process";
import process from "node:process";
import { pathToFileURL } from "node:url";
import { proxyPreviewApi } from "./preview-api-proxy.mjs";

const previewRoot = resolve(import.meta.dirname);
const desktopAssetsRoot = resolve(previewRoot, "../desktop/Resources/UI");

const contentTypes = {
  ".css": "text/css; charset=utf-8",
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".png": "image/png",
  ".svg": "image/svg+xml"
};

function resolveSafeAssetPath(root, requestPath) {
  const candidatePath = resolve(root, `.${requestPath}`);
  return candidatePath === root || candidatePath.startsWith(`${root}${sep}`) ? candidatePath : null;
}

export function createPreviewServer({ modelApi = process.env.KYNXA_MODEL_API_URL ?? "http://127.0.0.1:5218" } = {}) {
  return createServer(async (request, response) => {
    let url;
    let pathname;
    try {
      url = new URL(request.url ?? "/", "http://localhost");
      pathname = decodeURIComponent(url.pathname);
    } catch {
      response.writeHead(400).end("Invalid path");
      return;
    }
    if (url.pathname.startsWith("/api/")) {
      await proxyPreviewApi(request, response, url, modelApi);
      return;
    }
    let root = previewRoot;

    if (pathname.startsWith("/desktop-assets/")) {
      root = desktopAssetsRoot;
      pathname = pathname.slice("/desktop-assets".length);
    } else if (pathname === "/") {
      pathname = "/index.html";
    }

    const filePath = resolveSafeAssetPath(root, pathname);
    if (!filePath) {
      response.writeHead(400).end("Invalid path");
      return;
    }

    try {
      const assetContent = await readFile(filePath);
      response.writeHead(200, {
        "Content-Type": contentTypes[extname(filePath)] ?? "application/octet-stream",
        "Cache-Control": "no-cache"
      });
      response.end(assetContent);
    } catch (error) {
      response.writeHead(error?.code === "ENOENT" ? 404 : 500).end("Not found");
    }
  });
}

function startPreviewServer() {
  const portArgument = process.argv.find((value) => value.startsWith("--port="));
  const port = Number(portArgument?.split("=")[1] ?? process.env.KYNXA_PREVIEW_PORT ?? 4173);
  const server = createPreviewServer();
  server.listen(port, "127.0.0.1", () => {
    const url = `http://127.0.0.1:${port}`;
    console.log(`KYNXA desktop preview: ${url}`);
    console.log("Press Ctrl+C to stop.");

    if (process.argv.includes("--open")) {
      const opener = process.platform === "darwin" ? "open" : "xdg-open";
      const browser = spawn(opener, [url], { detached: true, stdio: "ignore" });
      browser.once("error", () => console.warn(`Open the preview manually: ${url}`));
      browser.unref();
    }
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) startPreviewServer();
