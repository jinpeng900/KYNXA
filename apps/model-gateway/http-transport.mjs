// Desktop HTTP delivery is separate from routing, generation and persistence.
export function sendJson(response, status, value) {
  response.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store' });
  response.end(JSON.stringify(value));
}

export async function readJsonBody(request, limit = 64 * 1024) {
  const chunks = [];
  let bytes = 0;
  for await (const chunk of request) {
    bytes += chunk.length;
    if (bytes > limit) throw new Error('请求体过大。');
    chunks.push(chunk);
  }
  try { return JSON.parse(Buffer.concat(chunks).toString('utf8')); }
  catch { throw new Error('请求体不是有效 JSON。'); }
}

// The caller owns generation and shutdown; this owns only the HTTP response.
export function openEventStream(response, identity, controller) {
  const cancel = () => { if (!response.writableEnded) controller.abort(); };
  response.on('close', cancel);
  response.on('error', cancel);
  const emit = event => {
    if (response.destroyed || response.writableEnded) return;
    if (response.writableLength > 1024 * 1024) {
      controller.abort(); response.destroy(); return;
    }
    response.write(`event: ${event.type}\ndata: ${JSON.stringify({ ...identity, ...event })}\n\n`);
  };
  response.writeHead(200, { 'Content-Type': 'text/event-stream; charset=utf-8',
    'Cache-Control': 'no-cache, no-transform', 'X-Accel-Buffering': 'no' });
  response.flushHeaders();
  const heartbeat = setInterval(() => {
    if (!response.destroyed && !response.writableEnded) response.write(': keep-alive\n\n');
  }, 15000);
  heartbeat.unref();
  return {
    emit,
    end() {
      clearInterval(heartbeat);
      response.off('close', cancel); response.off('error', cancel);
      response.end();
    }
  };
}
