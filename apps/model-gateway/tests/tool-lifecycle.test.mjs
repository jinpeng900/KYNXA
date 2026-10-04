import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ToolService } from '../tool-service.mjs';
import { ModelRuntime } from '../runtime.mjs';
import { pendingApproval, toolFixture } from './tool-fixture.mjs';

test('failed tool teardown still closes every owner, cancels approvals and completes snapshot cleanup once', async t => {
  const f = await toolFixture(t), closed = [];
  let releaseHost;
  const hostDone = new Promise(resolve => { releaseHost = resolve; });
  const failure = Object.assign(new Error('Synthetic desktop teardown failure.'), { code: 'DESKTOP_CLEANUP_FAILED' });
  const service = new ToolService({ conversationStore: f.conversations, dataHome: f.dataHome, bundledDirectory: null,
    desktopRunner: { close: async () => { closed.push('desktop'); throw failure; } },
    hostTerminalRunner: { close: async () => { await hostDone; closed.push('host'); } },
    sandboxRunner: { cleanupAll: async () => { assert.ok(closed.includes('host')); closed.push('sandbox'); } } });
  service.mcp.close = async () => { closed.push('mcp'); };
  const context = await service.createContext(f.conversationId, { requestId: f.call('unused', {}).id, permissionMode: 'ask' });
  const approval = await pendingApproval(service, context, f.call('filesystem.mkdir', { path: 'not-created' }));
  let finished = false;
  const closing = service.close().finally(() => { finished = true; });
  const checked = assert.rejects(closing, error => error === failure);
  assert.equal((await approval.result).code, 'TOOL_SERVICE_CLOSED');
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(finished, false);
  assert.deepEqual(closed.sort(), ['desktop', 'mcp']);
  releaseHost();
  await checked;
  assert.deepEqual(closed.sort(), ['desktop', 'host', 'mcp', 'sandbox']);
  await assert.rejects(service.close(), error => error === failure);
  assert.equal(closed.length, 4);
  assert.equal(service.approvals.pending.size, 0);
});

test('runtime retirement waits for request receipts even when a tool owner fails to close', async () => {
  const failure = Object.assign(new Error('Synthetic process cleanup failure.'), { code: 'MCP_PROCESS_CLEANUP_FAILED' });
  const runtime = new ModelRuntime({ modelStore: {}, conversationStore: { root: process.cwd() }, memoryService: {},
    toolService: { close: async () => { throw failure; } } });
  let release, saved = false, retired = false;
  runtime.queues.set('synthetic-request', new Promise(resolve => { release = () => { saved = true; resolve(); }; }));
  const closing = runtime.close().finally(() => { retired = true; });
  const checked = assert.rejects(closing, error => error === failure);
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(runtime.shutdown.signal.aborted, true);
  assert.equal(retired, false);
  release();
  await checked;
  assert.equal(saved, true);
  assert.equal(retired, true);
});
