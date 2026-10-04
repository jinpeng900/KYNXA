import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import test from "node:test";
import { createPreviewServer } from "../server.mjs";

async function listen(server, context) {
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  context.after(() => {
    server.closeAllConnections();
    return new Promise((resolve) => server.close(resolve));
  });
  return `http://127.0.0.1:${server.address().port}`;
}

test("preview preserves API query, request body and response type", async (context) => {
  const upstream = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    response.writeHead(201, { "Content-Type": "text/plain; charset=utf-8" });
    response.end(`${request.url}\n${Buffer.concat(chunks).toString()}`);
  });
  const modelApi = await listen(upstream, context);
  const preview = await listen(createPreviewServer({ modelApi }), context);
  const response = await fetch(`${preview}/api/example?offset=12&limit=24`, { method: "POST", body: "fixture", headers: { "Content-Type": "text/plain" } });
  assert.equal(response.status, 201);
  assert.equal(response.headers.get("content-type"), "text/plain; charset=utf-8");
  assert.equal(await response.text(), "/api/example?offset=12&limit=24\nfixture");
});

test("preview streams before completion and cancels disconnected upstream", async (context) => {
  let ended = false;
  let closed;
  const disconnected = new Promise((resolve) => { closed = resolve; });
  const upstream = createServer((_request, response) => {
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    response.write('data: {"type":"fixture"}\n\n');
    response.once("close", () => { ended = true; closed(); });
  });
  const modelApi = await listen(upstream, context);
  const preview = await listen(createPreviewServer({ modelApi }), context);
  const response = await fetch(`${preview}/api/chat/stream`, { signal: AbortSignal.timeout(5000) });
  assert.equal(response.headers.get("content-type"), "text/event-stream");
  const reader = response.body.getReader();
  assert.match(new TextDecoder().decode((await reader.read()).value), /fixture/);
  assert.equal(ended, false);
  await reader.cancel();
  await Promise.race([disconnected, new Promise((_resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("Preview did not cancel the upstream stream.")), 3000);
    disconnected.then(() => clearTimeout(timeout));
  })]);
  assert.equal(ended, true);
});

test("invalid URL escapes return 400 without stopping preview", async (context) => {
  const preview = await listen(createPreviewServer(), context);
  assert.equal((await fetch(`${preview}/%E0%A4%A`)).status, 400);
  const response = await fetch(preview);
  assert.equal(response.status, 200);
  assert.match(await response.text(), /KYNXA/);
});

test("unavailable gateway returns a bounded API error", async (context) => {
  const unavailable = createServer();
  unavailable.listen(0, "127.0.0.1");
  await once(unavailable, "listening");
  const modelApi = `http://127.0.0.1:${unavailable.address().port}`;
  await new Promise((resolve) => unavailable.close(resolve));
  const preview = await listen(createPreviewServer({ modelApi }), context);
  const response = await fetch(`${preview}/api/chat`);
  assert.equal(response.status, 502);
  assert.ok((await response.json()).error);
});
