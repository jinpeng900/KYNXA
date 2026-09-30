// Isolated upstream + real gateway for native desktop streaming checks. No provider credentials.
// Usage: node tests/streaming-ui-fixture.mjs <empty temporary data directory>
import { createServer } from 'node:http';
import { mkdir, writeFile, readdir } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { ModelStore } from '../apps/model-gateway/store.mjs';
import { ModelRuntime } from '../apps/model-gateway/runtime.mjs';
import { createModelServer } from '../apps/model-gateway/server.mjs';

if (!process.argv[2]) throw new Error('A temporary data directory is required.');
const dataRoot = resolve(process.argv[2]);
const existing = await readdir(dataRoot).catch(error => { if (error.code === 'ENOENT') return []; throw error; });
if (existing.length) throw new Error('Use an empty temporary data directory; existing files are never overwritten.');
await mkdir(join(dataRoot, 'Desktop'), { recursive: true });
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const upstream = createServer(async (request, response) => {
  let raw = '';
  for await (const chunk of request) raw += chunk;
  const input = JSON.parse(raw);
  const stopCase = input.messages.at(-1).content.includes('stop');
  response.writeHead(200, { 'Content-Type': 'text/event-stream' });
  const send = delta => response.write(`data: ${JSON.stringify({ choices: [{ delta }] })}\n\n`);
  for (const text of ['先梳理任务。\n\n', '公式：$1+1=2$。\n\n', '**检查完成**，准备给出答案。']) {
    if (response.destroyed) return;
    send({ reasoning_content: text }); await pause(450);
  }
  const sample = '这是流式输出的演示。\n\n## 结果\n\n文字会逐步出现，支持 **加粗** 和公式 $x^2+y^2=z^2$。\n\n```python\nprint("你好")\n```\n\n| 项目 | 状态 |\n| --- | --- |\n| 正文 | 已完成 |\n';
  const longCase = input.messages.at(-1).content.includes('follow');
  const body = longCase ? sample.repeat(16) : sample;
  const step = longCase ? 12 : 5;
  for (let offset = 0; offset < body.length; offset += step) {
    if (response.destroyed) return;
    send({ content: body.slice(offset, offset + step) }); await pause(stopCase ? 450 : 80);
  }
  response.write(`data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: 'stop' }] })}\n\n`);
  response.end('data: [DONE]\n\n');
});
await new Promise(resolve => upstream.listen(0, '127.0.0.1', resolve));
const models = join(dataRoot, 'Models');
const store = new ModelStore({ dataHome: models });
await store.save({ providerId: 'stream-test', displayName: '流式测试', baseUrl: `http://127.0.0.1:${upstream.address().port}/v1`, models: ['stream-fixture'] });
await writeFile(join(dataRoot, 'Desktop', 'model-selection.json'), JSON.stringify({ ProviderId: 'stream-test', ProviderName: '流式测试', ModelId: 'stream-fixture' }));
await writeFile(join(dataRoot, 'Desktop', 'projects.json'), '[]');
await writeFile(join(dataRoot, 'Desktop', 'chats.json'), '[]');
const runtime = new ModelRuntime({ modelStore: store, dataHome: models });
const server = createModelServer({ modelStore: store, modelRuntime: runtime });
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
await writeFile(join(dataRoot, 'fixture.json'), JSON.stringify({ gateway: `http://127.0.0.1:${server.address().port}`, pid: process.pid }));
console.log(JSON.stringify({ dataRoot, gateway: `http://127.0.0.1:${server.address().port}` }));
