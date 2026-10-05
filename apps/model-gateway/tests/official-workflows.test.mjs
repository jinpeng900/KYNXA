import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { mkdtemp, mkdir, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { ConversationStore } from '../data/conversations.mjs';
import { ToolService } from '../tools/tool-service.mjs';
import { ModelStore } from '../models/store.mjs';
import { ModelRuntime } from '../orchestration/runtime.mjs';
import { SandboxRunner } from '../tools/sandbox-runner.mjs';
import { createModelServer } from '../server.mjs';
import { readSse } from '../models/streaming.mjs';
import { curatedMcpPresets } from '../tools/official-tools.mjs';
import { isolateFixtureMcpCatalog } from './tool-fixture.mjs';

const protocols = ['openai-completions', 'openai-responses', 'anthropic-messages'];
const sourceUrls = ['https://sources.example.test/release', 'https://sources.example.test/changelog'];
const sourceAnswer = 'Version 2.1.0 was released on 2026-10-01. [Release](' + sourceUrls[0] + ') [Changelog](' + sourceUrls[1] + ')';
const mcpFixture = fileURLToPath(new URL('./fixtures/official-workflow-mcp.mjs', import.meta.url));

async function listen(server) {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  return 'http://127.0.0.1:' + server.address().port;
}
async function close(server) { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
function wireName(body, name) {
  const tool = body.tools.find(item => (item.description ?? item.function?.description ?? '').startsWith(name + ':'));
  assert.ok(tool, 'The actual upstream request declares ' + name);
  return tool.name ?? tool.function.name;
}
function toolsReply(protocol, body, calls, step) {
  const text = 'Public workflow step ' + step + '.';
  if (protocol === 'anthropic-messages') return { stop_reason: 'tool_use', content: [
    { type: 'thinking', thinking: 'Public workflow summary', signature: 'PRIVATE_WORKFLOW_SIGNATURE_' + step },
    { type: 'text', text }, ...calls.map(call => ({ type: 'tool_use', id: call.id, name: wireName(body, call.name), input: call.args }))] };
  if (protocol === 'openai-responses') return { status: 'completed', output: [
    { type: 'reasoning', id: 'reasoning_' + step, encrypted_content: 'PRIVATE_WORKFLOW_ENCRYPTED_' + step, summary: [] },
    { type: 'message', role: 'assistant', content: [{ type: 'output_text', text }] },
    ...calls.map(call => ({ type: 'function_call', id: 'function_' + call.id, call_id: call.id, name: wireName(body, call.name), arguments: JSON.stringify(call.args) }))] };
  return { choices: [{ finish_reason: 'tool_calls', message: { role: 'assistant', content: text,
    tool_calls: calls.map(call => ({ type: 'function', id: call.id, function: { name: wireName(body, call.name), arguments: JSON.stringify(call.args) } })) } }] };
}
function textReply(protocol, text) {
  if (protocol === 'anthropic-messages') return { stop_reason: 'end_turn', content: [{ type: 'text', text }] };
  if (protocol === 'openai-responses') return { status: 'completed', output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text }] }] };
  return { choices: [{ finish_reason: 'stop', message: { role: 'assistant', content: text } }] };
}
function pairs(body, protocol, expected) {
  const messages = body.messages ?? body.input;
  let calls, results, outputs;
  if (protocol === 'anthropic-messages') {
    const blocks = messages.flatMap(item => Array.isArray(item.content) ? item.content : []);
    calls = blocks.filter(item => item.type === 'tool_use').map(item => item.id);
    const observations = blocks.filter(item => item.type === 'tool_result');
    results = observations.map(item => item.tool_use_id); outputs = observations.map(item => item.content);
  } else if (protocol === 'openai-responses') {
    calls = messages.filter(item => item.type === 'function_call').map(item => item.call_id);
    const observations = messages.filter(item => item.type === 'function_call_output');
    results = observations.map(item => item.call_id); outputs = observations.map(item => item.output);
  } else {
    calls = messages.flatMap(item => (item.tool_calls ?? []).map(call => call.id));
    const observations = messages.filter(item => item.role === 'tool');
    results = observations.map(item => item.tool_call_id); outputs = observations.map(item => item.content);
  }
  assert.equal(calls.length, expected); assert.deepEqual(results, calls); assert.equal(new Set(calls).size, expected);
  return { messages, calls, outputs };
}
function observationValue(text) {
  const result = JSON.parse(text);
  // Current-step previews and reloaded canonical typed archives are both legitimate observations.
  // 当前步骤的预览和重新加载的正式带类型归档都是有效的观测结果。
  if (result.structuredContent !== undefined) return result.structuredContent;
  const block = Array.isArray(result.content) ? result.content.find(item => item.type === 'text') : null;
  return block ? JSON.parse(block.text) : result;
}

/** All stores, native snapshots, MCP processes and network endpoints belong to this temporary fixture.
 * 所有存储、原生快照、MCP 进程和网络端点均属于此临时测试夹具。
 */
async function workflowFixture(t, protocol, { sandboxRunner, scenario, configure } = {}) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-official-workflow-'));
  const dataHome = join(root, 'Data', 'Models'), extensionRoot = join(root, 'Extensions'), workspace = join(root, 'Work');
  await Promise.all([mkdir(dataHome, { recursive: true }), mkdir(extensionRoot), mkdir(workspace)]);
  const conversationId = randomUUID(), projectId = randomUUID(), seen = [], upstreamErrors = [];
  let conversations = new ConversationStore({ dataHome, legacyDesktopDirectory: null });
  await conversations.saveCatalog({ ...(await conversations.catalog()), Projects: [{ Id: projectId, Name: 'Isolated workflow', FolderPath: workspace,
    Chats: [{ Id: conversationId, Title: 'Workflow', Messages: [{ Id: randomUUID(), Role: 'user',
      Content: 'Synthetic initialized work chat', Status: 'completed' }] }] }], Chats: [] });
  let service, gateway, address;
  const upstream = createServer(async (request, response) => {
    try {
      let raw = ''; for await (const chunk of request) raw += chunk;
      const body = JSON.parse(raw); seen.push(body);
      const result = await scenario(body, seen.length, protocol);
      response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(JSON.stringify(result));
    } catch (error) {
      upstreamErrors.push(error.message);
      response.writeHead(500, { 'Content-Type': 'application/json' }); response.end(JSON.stringify({ error: error.message }));
    }
  });
  const upstreamUrl = await listen(upstream), models = new ModelStore({ dataHome });
  await models.save({ providerId: 'workflow-fixture', displayName: 'Isolated workflow model', protocol, apiKey: 'FAKE_WORKFLOW_CREDENTIAL',
    baseUrl: upstreamUrl + '/v1', models: ['fixture-model'], contextWindowTokens: 65536, maxOutputTokens: 4096 });
  const start = async () => {
    service = new ToolService({ conversationStore: conversations, dataHome, extensionRoot, sandboxRunner, bundledDirectory: null, officialTools: true });
    isolateFixtureMcpCatalog(service);
    const runtime = new ModelRuntime({ modelStore: models, dataHome, extensionRoot, conversationStore: conversations, toolService: service });
    gateway = createModelServer({ modelStore: models, modelRuntime: runtime }); address = await listen(gateway);
  };
  const stop = async () => { if (gateway) { await gateway.shutdownModelRuntime(); await close(gateway); gateway = null; } };
  t.after(async () => {
    await stop(); await close(upstream);
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith('..' + sep));
    await rm(root, { recursive: true, force: true });
  });
  await start(); if (configure) await configure({ service, root, workspace, extensionRoot });
  // A new package may add enabled publisher defaults. This workflow only owns its explicitly replaced Node fixtures.
  // 新包可能新增默认启用的上游服务；本工作流仅运行显式替换的 Node 测试服务。
  const config = await service.getConfig();
  if (config.mcpServers.some(server => server.origin === 'official' && server.enabled && server.command !== process.execPath))
    await service.updateConfig({ ...config, expectedRevision: config.revision,
      mcpServers: config.mcpServers.map(server => server.origin === 'official' && server.command !== process.execPath
        ? { ...server, enabled: false } : server) });
  const input = message => ({ conversationId, requestId: randomUUID(), userMessageId: randomUUID(), provider: 'workflow-fixture',
    model: 'fixture-model', message, permissionMode: 'full' });
  const post = (path, payload) => fetch(address + path, { method: 'POST', body: JSON.stringify(payload) });
  async function stream(payload) {
    const response = await post('/api/chat/stream', payload); assert.equal(response.status, 200);
    const events = []; for await (const raw of readSse(response.body)) events.push(JSON.parse(raw.data));
    assert.equal(events.at(-1).type, 'completed', JSON.stringify({ terminal: events.at(-1), upstreamErrors }));
    assert.equal(events.at(-1).toolStreamProtocol, 3); return events;
  }
  const saved = async id => (await conversations.readMessages(conversationId)).find(item => item.Id === id);
  return { root, dataHome, extensionRoot, workspace, conversationId, seen, upstreamErrors, input, post, stream, saved,
    restart: async () => { await stop(); conversations = new ConversationStore({ dataHome, legacyDesktopDirectory: null }); await start(); },
    get: path => fetch(address + path) };
}

for (const protocol of protocols) test(protocol + ': search, read two returned source URLs, cite them, then reopen with paired evidence and no replay', async t => {
  let originalIds;
  const f = await workflowFixture(t, protocol, {
    configure: async ({ service, extensionRoot }) => {
      const config = await service.getConfig();
      assert.ok(curatedMcpPresets.length > 0);
      assert.equal(config.mcpServers.length, curatedMcpPresets.length);
      assert.ok(config.mcpServers.every(item => item.enabled === true), 'Fresh official defaults follow the enabled package configuration');
      const mcpServers = config.mcpServers.map(server => {
        if (server.id === 'official-exa') return { id: server.id, name: 'Fixture multi-source search', command: process.execPath,
          args: [mcpFixture, 'search', join(extensionRoot, 'search.jsonl')], enabled: true };
        if (server.id === 'official-fetch') return { id: server.id, name: 'Fixture source reader', command: process.execPath,
          args: [mcpFixture, 'fetch', join(extensionRoot, 'fetch.jsonl')], enabled: true };
        return { ...server, enabled: false };
      });
      assert.equal(mcpServers.filter(server => server.enabled).length, 2);
      await service.updateConfig({ ...config, expectedRevision: config.revision, mcpServers });
    },
    scenario: (body, round) => {
      const expected = round === 1 ? 0 : round === 2 ? 1 : 3;
      const history = pairs(body, protocol, expected);
      assert.doesNotMatch(JSON.stringify(body.messages ?? body.input), /FAKE_WORKFLOW_CREDENTIAL|PRIVATE_WORKFLOW_.*METADATA/);
      if (round === 1) return toolsReply(protocol, body, [{ id: 'search_1', name: 'mcp.official-exa.web_search_exa', args: {
        arguments: { query: 'Latest fixture release: use multiple official sources' }, policy: { reason: 'Find direct fixture source URLs.' } } }], round);
      if (round === 2) {
        const search = observationValue(history.outputs.at(-1)); assert.deepEqual(search.results.map(item => item.url), sourceUrls);
        return toolsReply(protocol, body, search.results.map((item, index) => ({ id: 'source_' + index, name: 'mcp.official-fetch.fetch',
          args: { arguments: { url: item.url }, policy: { reason: 'Read the source returned by the search.' } } })), round);
      }
      for (const url of sourceUrls) assert.ok(history.outputs.some(text => observationValue(text).url === url));
      if (round === 3) {
        originalIds = history.calls;
        if (protocol === 'anthropic-messages') assert.match(JSON.stringify(history.messages), /PRIVATE_WORKFLOW_SIGNATURE_1/);
        if (protocol === 'openai-responses') assert.match(JSON.stringify(history.messages), /PRIVATE_WORKFLOW_ENCRYPTED_1/);
        return textReply(protocol, sourceAnswer);
      }
      assert.equal(round, 4); assert.ok(history.calls.every(id => !originalIds.includes(id)), 'Cross-request IDs are namespaced');
      assert.doesNotMatch(JSON.stringify(history.messages), /PRIVATE_WORKFLOW_SIGNATURE|PRIVATE_WORKFLOW_ENCRYPTED/);
      return textReply(protocol, 'The previous answer already used both sources. ' + sourceAnswer);
    }
  });
  const request = f.input('Search the latest fixture release and verify multiple sources.'), events = await f.stream(request);
  assert.equal(events.at(-1).content, sourceAnswer); assert.equal(events.at(-1).assistantSegments.at(-1).phase, 'final_answer');
  let saved = await f.saved(request.requestId);
  assert.equal(saved.Content, sourceAnswer); assert.equal(saved.Status, 'completed'); assert.equal(saved.ToolActivities.length, 3);
  assert.ok(saved.ToolActivities.every(item => item.status === 'completed' && item.resultRef));
  assert.equal(saved.ToolRun.diagnostics.executedToolCalls, 3); assert.equal(saved.ToolRun.diagnostics.modelCalls, 3);
  assert.doesNotMatch(JSON.stringify(saved), /PRIVATE_WORKFLOW_|FAKE_WORKFLOW_CREDENTIAL/);
  for (const tool of saved.ToolActivities) {
    const response = await f.get('/api/conversations/' + f.conversationId + '/tool-results/' + tool.resultRef.id);
    assert.equal(response.status, 200); assert.doesNotMatch(JSON.stringify(await response.json()), /PRIVATE_WORKFLOW_|FAKE_WORKFLOW_CREDENTIAL/);
  }
  const log = async name => (await readFile(join(f.extensionRoot, name + '.jsonl'), 'utf8')).trim().split('\n').map(line => JSON.parse(line));
  assert.equal((await log('search')).filter(item => item.event === 'search').length, 1);
  const fetchEvents = (await log('fetch')).filter(item => item.event === 'fetch');
  assert.deepEqual(fetchEvents.map(item => item.url).sort(), [...sourceUrls].sort());
  assert.equal(Math.max(...fetchEvents.map(item => item.active)), 2, 'The actual official fetch ID runs both independent reads concurrently');
  await f.restart(); assert.equal((await f.saved(request.requestId)).Content, sourceAnswer);
  const replay = await f.post('/api/chat', request); assert.equal(replay.status, 200); assert.equal((await replay.json()).content, sourceAnswer);
  assert.equal(f.seen.length, 3);
  const followup = await f.post('/api/chat', f.input('Which sources did the previous answer actually read?'));
  assert.equal(followup.status, 200, JSON.stringify(f.upstreamErrors)); assert.match((await followup.json()).content, /previous answer already used both sources/);
  assert.equal(f.seen.length, 4);
  assert.equal((await log('search')).filter(item => item.event === 'search').length, 1);
  assert.equal((await log('fetch')).filter(item => item.event === 'fetch').length, 2);
});

test('real Windows AppContainer workflow: read, hash edit, failing Node test, repair, passing test, final and reopened paired history',
  { skip: process.platform !== 'win32', timeout: 60000 }, async t => {
    const sandboxRunner = new SandboxRunner(), capabilities = await sandboxRunner.capabilities();
    if (!capabilities.available) { t.skip('Native AppContainer workflow not verified: ' + capabilities.reason); return; }
    assert.equal(sandboxRunner.verifiedAppContainer, true);
    const final = 'Fixed addition. The real AppContainer Node test now passes (1 test).';
    const testArgs = ['--test', '--test-isolation=none', 'calculator.test.cjs'];
    const f = await workflowFixture(t, 'openai-completions', { sandboxRunner,
      configure: async ({ workspace }) => {
        await writeFile(join(workspace, 'calculator.cjs'), 'exports.add = (a, b) => a - b;\n');
        await writeFile(join(workspace, 'calculator.test.cjs'), "const test = require('node:test'); const assert = require('node:assert/strict');\nconst { add } = require('./calculator.cjs');\ntest('addition', () => assert.equal(add(2, 3), 5));\n");
      },
      scenario: (body, round) => {
        const history = pairs(body, 'openai-completions', Math.min(round - 1, 6));
        const latest = history.outputs.length ? observationValue(history.outputs.at(-1)) : null;
        if (round === 1 || round === 4) {
          if (round === 4) { assert.notEqual(latest.exitCode, 0); assert.match(latest.stdout, /fail 1/); assert.equal(latest.tokenVerified, true); }
          return toolsReply('openai-completions', body, [{ id: 'code_' + round, name: 'filesystem.read', args: { path: 'calculator.cjs' } }], round);
        }
        if (round === 2 || round === 5) {
          assert.match(latest.sha256, /^[a-f0-9]{64}$/);
          return toolsReply('openai-completions', body, [{ id: 'code_' + round, name: 'filesystem.edit', args: { path: 'calculator.cjs',
            oldText: round === 2 ? 'a - b' : 'a * b', newText: round === 2 ? 'a * b' : 'a + b', expectedHash: latest.sha256 } }], round);
        }
        if (round === 3 || round === 6) return toolsReply('openai-completions', body, [{ id: 'code_' + round, name: 'terminal.run',
          args: { command: 'node', args: testArgs, timeoutMs: 15000 } }], round);
        const terminals = history.outputs.map(observationValue).filter(item => item.sandbox === 'appcontainer');
        assert.equal(terminals.length, 2); assert.notEqual(terminals[0].exitCode, 0); assert.equal(terminals[1].exitCode, 0);
        assert.match(terminals[1].stdout, /pass 1/); assert.ok(terminals.every(item => item.tokenVerified && item.activeProcessesAfterExit === 0));
        if (round === 8) assert.ok(history.calls.every(id => !id.startsWith('code_')), 'Persisted pairs are namespaced after reopening');
        else assert.equal(round, 7);
        return textReply('openai-completions', final);
      }
    });
    const request = { ...f.input('Fix addition, run tests, and keep repairing until the tests pass.'), permissionMode: 'smart' };
    const events = await f.stream(request), saved = await f.saved(request.requestId);
    assert.equal(events.at(-1).content, final); assert.equal(saved.Content, final); assert.equal(saved.Status, 'completed');
    assert.deepEqual(saved.ToolActivities.map(item => item.status), ['completed', 'completed', 'error', 'completed', 'completed', 'completed']);
    assert.ok(saved.ToolActivities.every(item => item.resultRef));
    assert.equal(saved.ToolRun.diagnostics.executedToolCalls, 6); assert.equal(saved.ToolRun.diagnostics.modelCalls, 7);
    assert.equal(await readFile(join(f.workspace, 'calculator.cjs'), 'utf8'), 'exports.add = (a, b) => a + b;\n');
    await f.restart(); assert.deepEqual((await f.saved(request.requestId)).ToolRun, saved.ToolRun);
    const followup = await f.post('/api/chat', f.input('Which test already failed and then passed?'));
    assert.equal(followup.status, 200); assert.equal((await followup.json()).content, final); assert.equal(f.seen.length, 8);
    assert.equal((await f.saved(request.requestId)).ToolActivities.length, 6);
  });
