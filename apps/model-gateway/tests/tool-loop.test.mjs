import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { after, test } from 'node:test';
import { ConversationStore } from '../conversations.mjs';
import { ModelStore } from '../store.mjs';
import { ModelRuntime } from '../runtime.mjs';
import { readSse } from '../streaming.mjs';
import { readToolStream } from '../tool-streaming.mjs';
import { wireCatalog } from '../tool-protocols.mjs';

const importRoot = await mkdtemp(join(tmpdir(), 'kynxa-tool-import-'));
const originalHome = process.env.KYNXA_DATA_HOME;
process.env.KYNXA_DATA_HOME = importRoot;
const { createModelServer } = await import('../server.mjs');
if (originalHome == null) delete process.env.KYNXA_DATA_HOME; else process.env.KYNXA_DATA_HOME = originalHome;
async function cleanup(root) {
  const suffix = relative(resolve(tmpdir()), resolve(root));
  assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
  await rm(root, { recursive: true, force: true });
}
after(() => cleanup(importRoot));
const frame = item => `data: ${typeof item === 'string' ? item : JSON.stringify(item)}\n\n`;
async function listen(server) { await new Promise(done => server.listen(0, '127.0.0.1', done)); return `http://127.0.0.1:${server.address().port}`; }
async function close(server) { server.closeAllConnections(); await new Promise(done => server.close(done)); }
const nativeTool = (protocol, name, args) => protocol === 'anthropic-messages'
  ? { stop_reason: 'tool_use', content: [{ type: 'thinking', thinking: 'Public summary', signature: 'private-fixture-signature' },
    { type: 'tool_use', id: 'call_1', name, input: args }] }
  : protocol === 'openai-responses'
    ? { status: 'completed', output: [{ type: 'reasoning', id: 'r_1', encrypted_content: 'private-fixture-reasoning', summary: [] },
      { type: 'function_call', id: 'f_1', call_id: 'call_1', name, arguments: JSON.stringify(args) }] }
    : { choices: [{ finish_reason: 'tool_calls', message: { role: 'assistant', content: '',
      reasoning_content: 'Public summary', tool_calls: [{ type: 'function', id: 'call_1', function: { name, arguments: JSON.stringify(args) } }] } }] };
const nativeText = (protocol, text) => protocol === 'anthropic-messages'
  ? { stop_reason: 'end_turn', content: [{ type: 'text', text }] }
  : protocol === 'openai-responses'
    ? { status: 'completed', output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text }] }] }
    : { choices: [{ finish_reason: 'stop', message: { role: 'assistant', content: text } }] };

async function fixture(t, protocol = 'openai-completions', operation = 'filesystem.read') {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-tool-e2e-'));
  const workspace = join(root, 'Work'); await mkdir(workspace);
  const marker = `FILE_${randomUUID()}`; await writeFile(join(workspace, 'note.txt'), marker);
  const seen = [], dataHome = join(root, 'Data', 'Models');
  let replayResponse = false, revisionFailure = false;
  const upstream = createServer(async (request, response) => {
    try {
      let raw = ''; for await (const chunk of request) raw += chunk;
      const body = JSON.parse(raw); seen.push(body);
      const history = body.messages ?? body.input;
      const toolResult = protocol === 'anthropic-messages' ? history.at(-1)?.content?.find?.(x => x.type === 'tool_result')?.content
        : protocol === 'openai-responses' ? history.findLast(x => x.type === 'function_call_output')?.output
          : history.findLast(x => x.role === 'tool')?.content;
      let result;
      if (toolResult != null && revisionFailure) { response.writeHead(503); response.end(); return; }
      if (toolResult != null && !replayResponse) {
        if (protocol === 'anthropic-messages') assert.equal(history.at(-2).content[0].signature, 'private-fixture-signature');
        if (protocol === 'openai-responses') assert.ok(history.some(x => x.encrypted_content === 'private-fixture-reasoning'));
        result = nativeText(protocol, `Observed tool result: ${toolResult}`);
      } else {
        const descriptor = body.tools.find(x => (x.description ?? x.function?.description).startsWith(operation + ':'));
        assert.ok(descriptor, 'model actually receives declared tool');
        result = nativeTool(protocol, descriptor.name ?? descriptor.function.name,
          operation === 'filesystem.write' ? { path: 'made.txt', content: 'via-tool', expectedHash: null }
            : operation === 'mcp.synthetic.echo' ? { value: marker, reason: 'Use the explicitly configured synthetic MCP service' }
              : { path: 'note.txt' });
      }
      if (revisionFailure && protocol === 'openai-responses') {
        result.output.push({ type: 'message', content: [{ type: 'output_text', text: 'REVISED TEXT' }] });
        response.writeHead(200, { 'Content-Type': 'text/event-stream' });
        response.end(frame({ type: 'response.output_text.delta', delta: 'DRAFT TEXT' })
          + frame({ type: 'response.completed', response: result }));
      } else { response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(JSON.stringify(result)); }
    } catch (error) { response.writeHead(500); response.end(JSON.stringify({ error: error.message })); }
  });
  const baseUrl = await listen(upstream); t.after(() => close(upstream));
  const models = new ModelStore({ dataHome });
  await models.save({ providerId: 'fixture-tools', displayName: 'Tools fixture', baseUrl: baseUrl + '/v1', protocol,
    models: ['tool-fixture'], contextWindowTokens: 32768 });
  const conversations = new ConversationStore({ dataHome });
  const id = randomUUID(), projectId = randomUUID();
  await conversations.saveCatalog({ Revision: (await conversations.catalog()).Revision, Projects: [{ Id: projectId, Name: 'Work', FolderPath: workspace,
    Chats: [{ Id: id, Title: 'Tools', Messages: [{ Id: randomUUID(), Role: 'user', Content: 'Synthetic initial message', Status: 'completed' }] }] }], Chats: [] });
  const runtime = new ModelRuntime({ modelStore: models, dataHome, conversationStore: conversations });
  const gateway = createModelServer({ modelStore: models, modelRuntime: runtime }); const address = await listen(gateway);
  t.after(async () => {
    try { await gateway.shutdownModelRuntime(); }
    finally { await close(gateway); await cleanup(root); }
  });
  const input = { conversationId: id, requestId: randomUUID(), userMessageId: randomUUID(), provider: 'fixture-tools',
    model: 'tool-fixture', message: 'Read the mounted file', permissionMode: 'ask' };
  return { root, runtime, conversations, seen, input, marker, workspace, address, setRepeat: value => { replayResponse = value; }, setRevisionFailure: value => { revisionFailure = value; } };
}

for (const protocol of ['openai-completions', 'openai-responses', 'anthropic-messages']) {
  test(`actual mounted filesystem result is continued through ${protocol}, persisted and replayed without execution`, async t => {
    const f = await fixture(t, protocol);
    const content = await f.runtime.reply(f.input);
    assert.ok(content.includes(f.marker), content); assert.equal(f.seen.length, 2);
    const saved = (await f.conversations.readMessages(f.input.conversationId)).at(-1);
    assert.equal(saved.ToolActivities[0].name, 'filesystem.read');
    assert.equal(saved.ToolActivities[0].status, 'completed');
    assert.ok(saved.ToolActivities[0].result.includes(f.marker));
    assert.ok(!JSON.stringify(saved).includes('private-fixture'));
    assert.equal(await f.runtime.reply(f.input), content); assert.equal(f.seen.length, 2);
    await assert.rejects(f.runtime.reply({ ...f.input, permissionMode: 'full' }), /权限/);
  });
}

test('a model discovers and calls a real official-SDK MCP server, then consumes its tool result', async t => {
  const f = await fixture(t, 'openai-completions', 'mcp.synthetic.echo');
  const log = join(f.root, 'mcp-events.jsonl');
  await f.runtime.tools.updateConfig({ version: 1, expectedRevision: 0, skillDirectories: [],
    mcpServers: [{ id: 'synthetic', name: 'Synthetic SDK service', command: process.execPath,
      args: [fileURLToPath(new URL('./fixtures/mcp-tool-server.mjs', import.meta.url)), log], enabled: true }] });
  const answer = await f.runtime.reply({ ...f.input, permissionMode: 'full' });
  assert.ok(answer.includes(`echo:${f.marker}`), answer);
  assert.equal(f.seen.length, 2);
  const effects = (await readFile(log, 'utf8')).trim().split('\n').map(line => JSON.parse(line));
  assert.equal(effects.filter(item => item.event === 'started').length, 1);
  assert.deepEqual(effects.filter(item => item.event === 'echo'), [{ event: 'echo', value: f.marker }]);
  const saved = (await f.conversations.readMessages(f.input.conversationId)).at(-1);
  assert.equal(saved.ToolActivities[0].name, 'mcp.synthetic.echo');
  assert.equal(saved.ToolActivities[0].status, 'completed');
  assert.equal(saved.ToolActivities[0].result, `echo:${f.marker}`);
});

test('connection edits during preparation cannot mix protocol, tool schema and continuation within a turn', async t => {
  for (const streaming of [false, true]) {
    const f = await fixture(t);
    const configured = await f.runtime.store.connectionFor(f.input.provider);
    let connectionReads = 0;
    f.runtime.store.connectionFor = async () => ++connectionReads === 1 ? configured
      : { ...configured, protocol: 'anthropic-messages', contextWindowTokens: 1024 };
    const answer = streaming ? (await f.runtime.replyStream(f.input, () => {})).content : await f.runtime.reply(f.input);
    assert.ok(answer.includes(f.marker), answer);
    assert.equal(connectionReads, 1);
    assert.equal(f.seen.length, 2);
    assert.ok(f.seen.every(body => body.tools[0].type === 'function' && body.tools[0].function.parameters));
  }
});

test('streamed Ask write waits for single-use bound approval; idle model timeout does not expire approval', async t => {
  const f = await fixture(t, 'openai-completions', 'filesystem.write');
  f.runtime.idleTimeoutMs = 20;
  const response = await fetch(f.address + '/api/chat/stream', { method: 'POST', body: JSON.stringify(f.input) });
  const events = []; let token;
  for await (const frame of readSse(response.body)) {
    const event = JSON.parse(frame.data); events.push(event);
    if (event.type === 'approval_required') {
      token = event.tool.approvalId;
      await assert.rejects(readFile(join(f.workspace, 'made.txt')), { code: 'ENOENT' });
      await new Promise(done => setTimeout(done, 50));
      const binding = { conversationId: f.input.conversationId, requestId: f.input.requestId,
        toolCallId: event.tool.toolCallId, approvalId: token, approved: true };
      let approved = await fetch(f.address + '/api/agent/approvals', { method: 'POST', body: JSON.stringify({ ...binding, requestId: randomUUID() }) });
      assert.equal(approved.status, 409);
      approved = await fetch(f.address + '/api/agent/approvals', { method: 'POST', body: JSON.stringify(binding) });
      assert.equal(approved.status, 200);
      const duplicate = await fetch(f.address + '/api/agent/approvals', { method: 'POST', body: JSON.stringify(binding) });
      assert.equal(duplicate.status, 404);
    }
  }
  assert.ok(token, JSON.stringify(events)); assert.equal(await readFile(join(f.workspace, 'made.txt'), 'utf8'), 'via-tool');
  assert.equal(events.at(-1).type, 'completed');
  assert.deepEqual(events.filter(x => x.type.startsWith('tool_')).map(x => x.type), ['tool_call', 'tool_result']);
});

test('noninteractive Ask write is denied; unknown/repeated call does not repeat effects on retry', async t => {
  const f = await fixture(t, 'openai-completions', 'filesystem.write');
  const answer = await f.runtime.reply(f.input); assert.ok(answer.includes('审批'), answer);
  await assert.rejects(readFile(join(f.workspace, 'made.txt')), { code: 'ENOENT' });
  const g = await fixture(t, 'openai-completions', 'filesystem.write'); g.setRepeat(true);
  const full = { ...g.input, permissionMode: 'full' };
  await assert.rejects(g.runtime.reply(full), /重复/);
  assert.equal(await readFile(join(g.workspace, 'made.txt'), 'utf8'), 'via-tool');
  const calls = g.seen.length;
  await assert.rejects(g.runtime.reply(full), /不能自动重做/); assert.equal(g.seen.length, calls);
});

test('partial streamed function arguments never execute; complete multi-fragment calls decode', async () => {
  const catalog = wireCatalog([{ name: 'filesystem.read', description: 'Read', inputSchema: { type: 'object' } }]);
  const first = { choices: [{ index: 0, delta: { tool_calls: [{ index: 0, id: 'call_1', function: { name: catalog[0].wireName, arguments: '{"path":' } }] } }] };
  await assert.rejects(readToolStream(new Response(frame(first), { headers: { 'content-type': 'text/event-stream' } }), 'openai-completions', catalog), /连接已断开/);
  const rest = { choices: [{ index: 0, delta: { tool_calls: [{ index: 0, function: { arguments: '"note.txt"}' } }] }, finish_reason: 'tool_calls' }] };
  const turn = await readToolStream(new Response(frame(first) + frame(rest), { headers: { 'content-type': 'text/event-stream' } }), 'openai-completions', catalog);
  assert.deepEqual(turn.calls[0], { id: 'call_1', name: 'filesystem.read', arguments: { path: 'note.txt' } });
});

test('browser Origin cannot invoke local model execution or change agent configuration', async t => {
  const f = await fixture(t);
  for (const path of ['/api/chat', '/api/agent/config', '/api/agent/mcp/refresh']) {
    const response = await fetch(f.address + path, { method: 'POST', headers: { Origin: 'http://127.0.0.1:8080' }, body: JSON.stringify(f.input) });
    assert.equal(response.status, 403);
  }
  assert.equal(f.seen.length, 0);
});


test('a completed-round revision survives tool continuation failure in SSE and the formal transcript', async t => {
  const f = await fixture(t, 'openai-responses'); f.setRevisionFailure(true);
  const response = await fetch(f.address + '/api/chat/stream', { method: 'POST', body: JSON.stringify(f.input) });
  const events = [];
  for await (const raw of readSse(response.body)) events.push(JSON.parse(raw.data));
  assert.equal(events.find(x => x.type === 'text_delta').delta, 'DRAFT TEXT');
  assert.equal(events.find(x => x.type === 'content_snapshot').content, 'REVISED TEXT');
  assert.equal(events.at(-1).type, 'error'); assert.equal(events.at(-1).content, 'REVISED TEXT');
  const saved = (await f.conversations.readMessages(f.input.conversationId)).at(-1);
  assert.equal(saved.Content, 'REVISED TEXT'); assert.equal(saved.ToolActivities[0].status, 'completed');
  assert.ok(!JSON.stringify(saved).includes('private-fixture-reasoning'));
  const before = f.seen.length;
  await assert.rejects(f.runtime.reply(f.input), /不能自动重做/); assert.equal(f.seen.length, before);
});

test('agent config, skills and cached-tool routes use revision checks without starting MCP on passive reads', async t => {
  const f = await fixture(t);
  let response = await fetch(f.address + '/api/agent/config');
  const config = await response.json(); assert.equal(config.revision, 0);
  const skillResponse = await fetch(f.address + '/api/agent/skills?conversationId=' + f.input.conversationId);
  const { skills } = await skillResponse.json(); assert.ok(skills.some(skill => skill.name === 'workspace-inspect'));
  const skill = skills.find(x => x.name === 'safe-file-edit');
  const detail = await (await fetch(f.address + '/api/agent/skills/' + skill.id)).json();
  assert.ok(detail.skill.content.includes('expectedHash'));
  const cached = await (await fetch(f.address + '/api/agent/tools')).json(); assert.ok(cached.tools.some(x => x.name === 'filesystem.read'));
  const next = { version: 1, expectedRevision: config.revision, mcpServers: [], skillDirectories: [] };
  response = await fetch(f.address + '/api/agent/config', { method: 'PUT', body: JSON.stringify(next) });
  assert.equal(response.status, 200); assert.equal((await response.json()).revision, 1);
  response = await fetch(f.address + '/api/agent/config', { method: 'PUT', body: JSON.stringify(next) }); assert.equal(response.status, 409);
  response = await fetch(f.address + '/api/agent/mcp/refresh', { method: 'POST' }); assert.equal(response.status, 200);
  assert.deepEqual((await response.json()).errors, []); assert.equal(f.seen.length, 0);
});
