import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { after, test } from 'node:test';
import { ConversationStore } from '../data/conversations.mjs';
import { ModelStore } from '../models/store.mjs';
import { ModelRuntime } from '../orchestration/runtime.mjs';
import { readSse } from '../models/streaming.mjs';
import { readToolStream } from '../models/tool-streaming.mjs';
import { wireCatalog } from '../models/tool-protocols.mjs';
import { isolateFixtureMcpCatalog } from './tool-fixture.mjs';

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
  let replayResponse = false, revisionFailure = false, plan;
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
      if (plan) result = await plan(body, marker, seen.length);
      else if (toolResult != null && !replayResponse) {
        if (protocol === 'anthropic-messages') assert.equal(history.at(-2).content[0].signature, 'private-fixture-signature');
        if (protocol === 'openai-responses') assert.ok(history.some(x => x.encrypted_content === 'private-fixture-reasoning'));
        result = nativeText(protocol, `Observed tool result: ${toolResult}`);
      } else {
        const descriptor = body.tools.find(x => (x.description ?? x.function?.description).startsWith(operation + ':'));
        assert.ok(descriptor, 'model actually receives declared tool');
        result = nativeTool(protocol, descriptor.name ?? descriptor.function.name,
          operation === 'filesystem.write' ? { path: 'made.txt', content: 'via-tool', expectedHash: null }
            : operation === 'mcp.synthetic.echo' ? { arguments: { value: marker }, policy: { reason: 'Use the explicitly configured synthetic MCP service' } }
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
    models: ['tool-fixture'], contextWindowTokens: 32768, maxOutputTokens: 2048 });
  const conversations = new ConversationStore({ dataHome });
  const id = randomUUID(), projectId = randomUUID();
  await conversations.saveCatalog({ Revision: (await conversations.catalog()).Revision, Projects: [{ Id: projectId, Name: 'Work', FolderPath: workspace,
    Chats: [{ Id: id, Title: 'Tools', Messages: [{ Id: randomUUID(), Role: 'user', Content: 'Synthetic initial message', Status: 'completed' }] }] }], Chats: [] });
  const runtime = new ModelRuntime({ modelStore: models, dataHome, conversationStore: conversations });
  isolateFixtureMcpCatalog(runtime.tools);
  const gateway = createModelServer({ modelStore: models, modelRuntime: runtime }); const address = await listen(gateway);
  t.after(async () => {
    try { await gateway.shutdownModelRuntime(); }
    finally { await close(gateway); await cleanup(root); }
  });
  const input = { conversationId: id, requestId: randomUUID(), userMessageId: randomUUID(), provider: 'fixture-tools',
    model: 'tool-fixture', message: 'Read the mounted file', permissionMode: 'ask' };
  return { root, runtime, conversations, seen, input, marker, workspace, address, setPlan: value => { plan = value; },
    setRepeat: value => { replayResponse = value; }, setRevisionFailure: value => { revisionFailure = value; } };
}

for (const protocol of ['openai-completions', 'openai-responses', 'anthropic-messages']) {
  test(`${protocol} displays final-only Content while preserving paired stages and observations in the next model context`, async t => {
    const f = await fixture(t, protocol);
    f.setPlan((body, _marker, round) => {
      if (round >= 3) return nativeText(protocol, 'Short final answer.');
      const descriptor = body.tools.find(item => (item.description ?? item.function?.description).startsWith('filesystem.read:'));
      const result = nativeTool(protocol, descriptor.name ?? descriptor.function.name, { path: 'note.txt' });
      if (protocol === 'anthropic-messages') {
        result.content.unshift({ type: 'text', text: `Stage ${round}.` });
        result.content.find(item => item.type === 'tool_use').id = `call_${round}`;
      } else if (protocol === 'openai-responses') {
        result.output.push({ type: 'message', content: [{ type: 'output_text', text: `Stage ${round}.` }] });
        result.output.find(item => item.type === 'function_call').call_id = `call_${round}`;
      } else {
        result.choices[0].message.content = `Stage ${round}.`;
        result.choices[0].message.tool_calls[0].id = `call_${round}`;
      }
      return result;
    });
    const response = await fetch(f.address + '/api/chat/stream', { method: 'POST', body: JSON.stringify(f.input) });
    const events = [];
    for await (const raw of readSse(response.body)) events.push(JSON.parse(raw.data));
    assert.equal(events.at(-1).type, 'completed');
    assert.equal(events.at(-1).content, 'Short final answer.');
    assert.equal(events.at(-1).toolStreamProtocol, 3);
    const segments = events.at(-1).assistantSegments;
    assert.deepEqual(segments.map(item => [item.round, item.order, item.phase]),
      [[1, 0, 'commentary'], [2, 2, 'commentary'], [3, 4, 'final_answer']]);
    assert.deepEqual(events.filter(item => item.type === 'tool_result').map(item => [item.tool.round, item.tool.order]), [[1, 1], [2, 3]]);
    const saved = (await f.conversations.readMessages(f.input.conversationId)).at(-1);
    assert.deepEqual(saved.AssistantSegments, segments);
    assert.equal(saved.Content, 'Short final answer.');
    assert.ok(!JSON.stringify(saved.AssistantSegments).includes('private-fixture'));
    const reloaded = new ConversationStore({ dataHome: join(f.root, 'Data', 'Models') });
    assert.deepEqual((await reloaded.readMessages(f.input.conversationId)).at(-1).AssistantSegments, segments);
    const turn = await f.runtime.prepare({ ...f.input, requestId: randomUUID(), userMessageId: randomUUID(), message: 'Next question.' }, f.input.conversationId);
    assert.ok(JSON.stringify(turn.messages).includes('Short final answer.'));
    assert.ok(JSON.stringify(turn.messages).includes('Stage 1.'));
    assert.ok(JSON.stringify(turn.messages).includes(f.marker));
    assert.ok(!JSON.stringify(turn.messages).includes('private-fixture'));
    assert.equal(saved.ModelTranscript, undefined);
    assert.equal((await reloaded.readModelMessages(f.input.conversationId)).find(item => item.Id === f.input.requestId).ModelTranscript.rounds.length, 3);
    await f.runtime.tools.releaseContext(turn.toolContext);
    assert.equal(f.seen.length, 3);
  });

  test(`actual mounted filesystem result is continued through ${protocol}, persisted and replayed without execution`, async t => {
    const f = await fixture(t, protocol);
    const content = await f.runtime.reply(f.input);
    assert.ok(content.includes(f.marker), content); assert.equal(f.seen.length, 2);
    const saved = (await f.conversations.readMessages(f.input.conversationId)).at(-1);
    assert.equal(saved.ToolActivities[0].name, 'filesystem.read');
    assert.equal(saved.ToolActivities[0].status, 'completed');
    assert.equal(saved.ToolRun.phase, 'completed');
    assert.equal(saved.ToolRun.rounds, 2);
    assert.equal(saved.ToolRun.toolCalls, 1);
    const runResponse = await fetch(f.address + '/api/conversations/' + f.input.conversationId + '/runs/' + f.input.requestId);
    assert.equal(runResponse.status, 200);
    assert.equal((await runResponse.json()).run.phase, 'completed');
    assert.ok(saved.ToolActivities[0].result.includes(f.marker));
    assert.ok(!JSON.stringify(saved).includes('private-fixture'));
    assert.equal(await f.runtime.reply(f.input), content); assert.equal(f.seen.length, 2);
    await assert.rejects(f.runtime.reply({ ...f.input, permissionMode: 'full' }), /权限/);
  });
}

for (const protocol of ['openai-completions', 'openai-responses', 'anthropic-messages']) {
  test(`real runtime ${protocol} continues its first large archived result through HTTP without replay`, async t => {
    const f = await fixture(t, protocol);
    const original = 'PUBLIC_CODE_SOURCE_' + 'A'.repeat(65000);
    await writeFile(join(f.workspace, 'large.txt'), original);
    let reference;
    f.setPlan(async (body, marker, round) => {
      const messages = body.messages ?? body.input;
      if (round === 1) {
        const descriptor = body.tools.find(tool => (tool.description ?? tool.function?.description).startsWith('filesystem.read:'));
        return nativeTool(protocol, descriptor.name ?? descriptor.function.name, { path: 'large.txt', maxChars: 64000 });
      }
      assert.equal(round, 2);
      const text = protocol === 'anthropic-messages' ? messages.at(-1).content.find(item => item.type === 'tool_result').content
        : protocol === 'openai-responses' ? messages.findLast(item => item.type === 'function_call_output').output
          : messages.findLast(item => item.role === 'tool').content;
      const compacted = JSON.parse(text);
      assert.equal(compacted.contextCompacted, true); reference = compacted.resultRef;
      assert.equal(compacted.navigation.tool, 'tool.result.read');
      if (protocol === 'anthropic-messages') assert.equal(messages.at(-2).content[0].signature, 'private-fixture-signature');
      if (protocol === 'openai-responses') assert.ok(messages.some(item => item.encrypted_content === 'private-fixture-reasoning'));
      let offset = 0, source = '';
      do {
        const page = await f.runtime.tools.results.read({ conversationId: f.input.conversationId }, reference.id, { offset, limit: 4096 });
        source += page.text; offset = page.nextOffset; if (!page.truncated) break;
      } while (true);
      assert.equal(JSON.parse(source).structuredContent.content, original.slice(0, 64000));
      return nativeText(protocol, 'Continued from the complete archived public result.');
    });
    assert.equal(await f.runtime.reply(f.input), 'Continued from the complete archived public result.');
    const saved = (await f.conversations.readMessages(f.input.conversationId)).at(-1);
    assert.equal(saved.ToolActivities.length, 1); assert.equal(saved.ToolActivities[0].resultRef.id, reference.id);
    assert.equal(saved.ToolActivities[0].status, 'completed'); assert.ok(!JSON.stringify(saved).includes('private-fixture'));
    assert.equal(await f.runtime.reply(f.input), saved.Content); assert.equal(f.seen.length, 2);
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

test('a real model HTTP loop discovers, loads and executes a deferred tool from a 100-tool MCP server', async t => {
  const f = await fixture(t);
  const target = 'mcp.synthetic.large_099';
  await f.runtime.tools.updateConfig({ version: 1, expectedRevision: 0, skillDirectories: [],
    mcpServers: [{ id: 'synthetic', name: 'Synthetic large catalog', command: process.execPath,
      args: [fileURLToPath(new URL('./fixtures/mcp-tool-server.mjs', import.meta.url)), join(f.root, 'many-events.jsonl'), 'many'], enabled: true }] });
  f.setPlan((body, marker, round) => {
    assert.ok(body.tools.length <= 96);
    const name = round === 1 ? 'tool.search' : round === 2 ? 'tool.load' : target;
    if (round === 4) return nativeText('openai-completions', body.messages.at(-1).content);
    if (round === 1) assert.ok(!body.tools.some(tool => tool.function.description.startsWith(target + ':')));
    const descriptor = body.tools.find(tool => tool.function.description.startsWith(name + ':'));
    assert.ok(descriptor, 'model receives the selected declaration in round ' + round);
    const args = round === 1 ? { query: 'large_099' } : round === 2 ? { names: [target] }
      : { arguments: { value: marker }, policy: { reason: 'Call the requested user-enabled synthetic tool' } };
    const result = nativeTool('openai-completions', descriptor.function.name, args);
    result.choices[0].message.tool_calls[0].id = 'call_' + round;
    return result;
  });
  const response = await fetch(f.address + '/api/chat/stream', { method: 'POST', body: JSON.stringify({ ...f.input, permissionMode: 'full' }) });
  const events = []; for await (const raw of readSse(response.body)) events.push(JSON.parse(raw.data));
  assert.equal(events.at(-1).type, 'completed'); assert.ok(events.at(-1).content.includes('large_099:' + f.marker));
  assert.equal(f.seen.length, 4);
  assert.deepEqual(events.filter(item => item.type === 'tool_result').map(item => item.tool.name), ['tool.search', 'tool.load', target]);
  const tool = events.find(item => item.type === 'tool_result' && item.tool.name === target).tool;
  assert.ok(tool.resultRef);
  const detail = await fetch(f.address + '/api/conversations/' + f.input.conversationId + '/tool-results/' + tool.resultRef.id);
  assert.equal(detail.status, 200); assert.ok((await detail.json()).result.content[0].text.includes(f.marker));
  const page = await fetch(f.address + '/api/conversations/' + f.input.conversationId + '/tool-results/' + tool.resultRef.id + '?offset=0&limit=32');
  assert.equal(page.status, 200); assert.equal((await page.json()).text.length, 32);
});

test('tight model budgets preserve plain chat when tool definitions cannot fit', async t => {
  const f = await fixture(t);
  const configured = await f.runtime.store.connectionFor(f.input.provider);
  f.runtime.store.connectionFor = async () => ({ ...configured, contextWindowTokens: 2048 });
  f.setPlan(body => { assert.equal(body.tools, undefined); return nativeText('openai-completions', 'Text fallback stays usable'); });
  const answer = await f.runtime.reply({ ...f.input, message: 'x'.repeat(1000) });
  assert.equal(answer, 'Text fallback stays usable'); assert.equal(f.seen.length, 1);
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

function withCallId(protocol, result, callId) {
  if (protocol === 'anthropic-messages') result.content.find(item => item.type === 'tool_use').id = callId;
  else if (protocol === 'openai-responses') result.output.find(item => item.type === 'function_call').call_id = callId;
  else result.choices[0].message.tool_calls[0].id = callId;
  return result;
}

for (const protocol of ['openai-completions', 'openai-responses', 'anthropic-messages']) {
  test(`${protocol}: an undeclared tool returns a paired failure, then a valid call and final answer continue`, async t => {
    const f = await fixture(t, protocol);
    const dispatched = [], originalExecute = f.runtime.tools.execute.bind(f.runtime.tools);
    f.runtime.tools.execute = async (...args) => { dispatched.push(args[1].name); return originalExecute(...args); };
    f.setPlan((body, marker, round) => {
      if (round === 1) return nativeTool(protocol, 'not_a_declared_tool', { path: 'must-not-execute.txt' });
      assert.ok(JSON.stringify(body.messages ?? body.input).includes('MODEL_TOOL_UNAVAILABLE'));
      if (round === 2) {
        const descriptor = body.tools.find(item => (item.description ?? item.function?.description).startsWith('filesystem.read:'));
        return withCallId(protocol, nativeTool(protocol, descriptor.name ?? descriptor.function.name, { path: 'note.txt' }), 'call_2');
      }
      return nativeText(protocol, `Recovered with verified result ${marker}`);
    });
    const response = await fetch(f.address + '/api/chat/stream', { method: 'POST', body: JSON.stringify(f.input) });
    const events = [];
    for await (const raw of readSse(response.body)) events.push(JSON.parse(raw.data));
    assert.equal(events.at(-1).type, 'completed'); assert.ok(events.at(-1).content.includes(f.marker));
    assert.deepEqual(dispatched, ['filesystem.read']);
    const saved = (await f.conversations.readMessages(f.input.conversationId)).at(-1);
    assert.equal(saved.Status, 'completed'); assert.equal(saved.ToolActivities[0].code, 'MODEL_TOOL_UNAVAILABLE');
    assert.equal(JSON.parse(saved.ToolActivities[0].result).executed, false);
    assert.equal(saved.ToolActivities[1].status, 'completed');
    assert.equal(saved.ToolRun.diagnostics.executedToolCalls, 1);
    const modelSaved = (await f.conversations.readModelMessages(f.input.conversationId)).at(-1);
    assert.equal(modelSaved.ModelTranscript.rounds[0].calls[0].name, 'not_a_declared_tool');
    assert.equal(modelSaved.ModelTranscript.rounds.length, 3);
  });

  test(`${protocol}: repeated unavailable tools finish normally, even when the provider ignores no-tools finalization`, async t => {
    const f = await fixture(t, protocol);
    f.runtime.tools.execute = async () => assert.fail('undeclared tools must never reach the execution broker');
    f.setPlan((_body, _marker, round) => withCallId(protocol,
      nativeTool(protocol, 'unavailable_browser_launcher', {}), `unavailable_${round}`));
    const result = await f.runtime.reply(f.input);
    assert.match(result, /unavailable/); assert.equal(f.seen.length, 3);
    assert.deepEqual(f.seen.at(-1).tools ?? [], []);
    const saved = (await f.conversations.readMessages(f.input.conversationId)).at(-1);
    assert.equal(saved.Status, 'completed'); assert.equal(saved.ToolRun.phase, 'completed');
    assert.equal(saved.ToolActivities.length, 3);
    assert.ok(saved.ToolActivities.every(item => item.code === 'MODEL_TOOL_UNAVAILABLE'));
    assert.equal(saved.ToolRun.diagnostics.executedToolCalls, 0);
    assert.equal(saved.AssistantSegments.at(-1).phase, 'final_answer');
    const next = await f.runtime.prepare({ ...f.input, requestId: randomUUID(), userMessageId: randomUUID(),
      message: 'Next question in the same conversation.' }, f.input.conversationId);
    assert.ok(JSON.stringify(next.messages).includes(saved.Content), 'the honest final limitation remains in future history');
    await f.runtime.tools.releaseContext(next.toolContext);
  });
}

test('a round decodes its exact declared catalog when availability changes while the response is generated', async t => {
  const f = await fixture(t);
  const originalCatalog = f.runtime.tools.modelCatalog.bind(f.runtime.tools);
  let expired = false;
  f.runtime.tools.modelCatalog = context => expired ? [] : originalCatalog(context);
  f.setPlan((body, marker, round) => {
    if (round > 1) return nativeText('openai-completions', `Verified ${marker}`);
    const descriptor = body.tools.find(item => item.function.description.startsWith('filesystem.read:'));
    expired = true;
    return nativeTool('openai-completions', descriptor.function.name, { path: 'note.txt' });
  });
  assert.ok((await f.runtime.reply(f.input)).includes(f.marker));
  const saved = (await f.conversations.readMessages(f.input.conversationId)).at(-1);
  assert.equal(saved.ToolActivities[0].name, 'filesystem.read');
  assert.equal(saved.ToolActivities[0].status, 'completed');
});

test('two searches, blocked public reads and a stale exhausted tool name still yield a normal final answer', async t => {
  const f = await fixture(t);
  let searchWireName;
  const originalExecute = f.runtime.tools.execute.bind(f.runtime.tools);
  f.runtime.tools.execute = async (context, call, options) => {
    if (call.name === 'web.search') {
      await f.runtime.tools.webSearch.take(context, 'query');
      return { content: JSON.stringify({ sources: [{ url: 'https://example.com/official', title: 'Dated official source',
        excerpt: 'Synthetic search evidence only; not a verified full page.' }] }), status: 'completed', isError: false };
    }
    if (call.name === 'web.fetch') return { content: JSON.stringify({ code: 'WEB_URL_BLOCKED',
      message: 'Synthetic non-public DNS answer. The full page has not been read.' }),
    code: 'WEB_URL_BLOCKED', status: 'error', isError: true };
    return originalExecute(context, call, options);
  };
  f.setPlan((body, _marker, round) => {
    if (round === 3) {
      assert.ok(!body.tools.some(item => item.function.name === searchWireName));
      return withCallId('openai-completions', nativeTool('openai-completions', searchWireName,
        { query: 'Try the exhausted search again', reason: 'Synthetic retry' }), 'stale_search');
    }
    if (round > 3) return nativeText('openai-completions', 'Search evidence is preserved, but the official pages could not be verified.');
    const toolName = round === 1 ? 'web.search' : 'web.fetch';
    const descriptor = body.tools.find(item => item.function.description.startsWith(toolName + ':'));
    if (round === 1) searchWireName = descriptor.function.name;
    const args = round === 1 ? { query: 'Current official announcement', reason: 'Synthetic query' }
      : { url: 'https://example.com/official', reason: 'Synthetic read' };
    const result = withCallId('openai-completions', nativeTool('openai-completions', descriptor.function.name, args), `round_${round}_a`);
    result.choices[0].message.tool_calls.push(withCallId('openai-completions',
      nativeTool('openai-completions', descriptor.function.name, args), `round_${round}_b`).choices[0].message.tool_calls[0]);
    return result;
  });
  const content = await f.runtime.reply({ ...f.input, message: '查证最新的官方公告', permissionMode: 'full' });
  assert.match(content, /could not be verified/); assert.equal(f.seen.length, 4);
  const saved = (await f.conversations.readMessages(f.input.conversationId)).at(-1);
  assert.equal(saved.Status, 'completed');
  assert.deepEqual(saved.ToolActivities.map(item => item.code ?? null),
    [null, null, 'WEB_URL_BLOCKED', 'WEB_URL_BLOCKED', 'MODEL_TOOL_UNAVAILABLE']);
  assert.equal(JSON.parse(saved.ToolActivities.at(-1).result).executed, false);
  const next = await f.runtime.prepare({ ...f.input, requestId: randomUUID(), userMessageId: randomUUID(), message: 'Continue the same task.' }, f.input.conversationId);
  assert.ok(JSON.stringify(next.messages).includes('MODEL_TOOL_UNAVAILABLE'));
  await f.runtime.tools.releaseContext(next.toolContext);
});
