import assert from 'node:assert/strict';
import { test } from 'node:test';
import { randomUUID } from 'node:crypto';
import { readFile, mkdtemp, realpath } from 'node:fs/promises';
import { join } from 'node:path';
import { HostTerminalRunner } from '../host-terminal-runner.mjs';
import { toolFixture, pendingApproval, approve, parsed } from './tool-fixture.mjs';
import { ModelToolCatalog } from '../tool-catalog.mjs';
import { builtinDescriptors } from '../official-tools/Tools/catalog.mjs';
import { runToolLoop } from '../tool-loop.mjs';

const capabilities = { protocolVersion: 1, boundary: 'host-terminal', available: true, sandbox: false,
  processTreeBounded: true, shells: ['cmd', 'powershell'] };
const input = { shell: 'cmd', script: 'echo fixture', reason: 'Run only this synthetic command.' };
function stub() {
  const calls = [];
  return { calls, capabilities: async () => capabilities, run: async request => {
    calls.push(request); return { value: { completed: true, boundary: 'host-terminal', stdout: 'fixture', exitCode: 0 }, isError: false };
  } };
}

test('host commands require Ask/Smart approval, never use the sandbox, and retain immutable arguments', async t => {
  const runner = stub(), f = await toolFixture(t, { hostTerminalRunner: runner });
  for (const mode of ['ask', 'smart']) {
    const ctx = await f.context(mode);
    assert.equal((await f.run(ctx, 'terminal.host.run', input, { interactive: false })).code, 'TOOL_APPROVAL_REQUIRED');
    const call = f.call('terminal.host.run', { ...input });
    const pending = await pendingApproval(f.service, ctx, call);
    assert.equal(pending.event.tool.outsideWorkspace, true);
    call.arguments.script = 'mutated';
    approve(f.service, ctx, pending.event.tool, mode === 'smart');
    assert.equal((await pending.result).code, mode === 'ask' ? 'TOOL_DENIED' : undefined);
  }
  assert.equal(runner.calls.length, 1); assert.equal(runner.calls[0].script, 'echo fixture');
  assert.equal(runner.calls[0].reason, undefined);
  assert.equal(runner.calls[0].cwd, f.workspace);
});

test('full host commands validate reasons and schemas; changed work/config cancels an approval', async t => {
  const runner = stub(), f = await toolFixture(t, { hostTerminalRunner: runner });
  const ctx = await f.context('full');
  assert.equal((await f.run(ctx, 'terminal.host.run', { ...input, reason: '   ' })).code, 'OUTSIDE_WORKSPACE_REASON_REQUIRED');
  assert.equal((await f.run(ctx, 'terminal.host.run', { ...input, shell: 'bash' })).isError, true);
  const success = await f.run(ctx, 'terminal.host.run', input);
  assert.equal(success.status, 'completed'); assert.ok(success.resultRef);
  const ask = await f.context('ask'); await f.service.catalog(ask);
  const pending = await pendingApproval(f.service, ask, f.call('terminal.host.run', input));
  const catalog = await f.conversations.catalog(); catalog.Projects[0].FolderPath = null;
  await f.conversations.saveCatalog(catalog); approve(f.service, ask, pending.event.tool);
  assert.equal((await pending.result).code, 'WORKSPACE_CHANGED');
  const next = await f.context('ask'); await f.service.catalog(next);
  const waiting = await pendingApproval(f.service, next, f.call('terminal.host.run', input));
  const config = await f.service.getConfig();
  await f.service.updateConfig({ ...config, expectedRevision: config.revision, skillDirectories: [join(f.root, 'skills')] });
  approve(f.service, next, waiting.event.tool);
  assert.equal((await waiting.result).code, 'AGENT_CONFIG_CHANGED');
  assert.equal(runner.calls.length, 1);
});

test('unavailable host terminal stays unavailable while a working sandbox never gets called', async t => {
  let sandboxCalls = 0;
  const f = await toolFixture(t, { sandboxRunner: { capabilities: async () => ({ available: true }), run: async () => sandboxCalls++ } });
  assert.equal((await f.run(await f.context('full'), 'terminal.host.run', input)).code, 'HOST_TERMINAL_UNAVAILABLE');
  assert.equal(sandboxCalls, 0);
});

test('visible host commands preserve approved mode and hold without falling back to launch or sandbox', async t => {
  const runner = stub();
  runner.capabilities = async () => ({ ...capabilities, protocolVersion: 2, visibleTerminal: true });
  const f = await toolFixture(t, { hostTerminalRunner: runner });
  for (const mode of ['ask', 'smart']) {
    const ctx = await f.context(mode), call = f.call('terminal.host.run', { ...input, visible: true, keepOpenMs: 2500 });
    const pending = await pendingApproval(f.service, ctx, call);
    assert.equal(pending.event.tool.arguments.visible, true);
    call.arguments.visible = false; call.arguments.keepOpenMs = 0;
    approve(f.service, ctx, pending.event.tool);
    assert.equal((await pending.result).status, 'completed');
    assert.equal(runner.calls.at(-1).visible, true); assert.equal(runner.calls.at(-1).keepOpenMs, 2500);
  }
  const ctx = await f.context('full');
  assert.equal((await f.run(ctx, 'terminal.host.run', { ...input, keepOpenMs: 1 })).code, 'HOST_TERMINAL_INVALID_REQUEST');
  assert.equal((await f.run(ctx, 'terminal.host.run', { ...input, visible: true, keepOpenMs: 30001 })).isError, true);
  runner.capabilities = async () => capabilities;
  assert.equal((await f.run(await f.context('full'), 'terminal.host.run', { ...input, visible: true })).code,
    'HOST_TERMINAL_VISIBLE_UNAVAILABLE');
  assert.equal(runner.calls.length, 2);
});

test('runner rejects old helper before dispatch and checks genuine visible console receipts',
  { skip: process.platform !== 'win32' }, async t => {
    const f = await toolFixture(t), ctx = await f.context('full');
    let executions = 0, currentCapabilities = capabilities;
    let receipt = {};
    const runner = new HostTerminalRunner({ toolHostPath: process.execPath, invoke: async (_, request) => {
      if (request.operation === 'host_terminal_capabilities') return currentCapabilities;
      assert.equal(request.operation, 'host_terminal_visible');
      executions++;
      return { protocolVersion: 2, boundary: 'host-terminal', completed: true, outcome: 'completed',
        shell: request.shell, cwd: request.cwd, exitCode: 0, stdout: '', stderr: '', activeProcessesAfterExit: 0,
        visibleRequested: true, windowObserved: true, windowId: '12345', windowProcessId: 123, consoleInput: 'console',
        consoleOutput: 'console', outputCapture: 'console-screen', commandCompleted: true,
        exitCodeObserved: true, consoleSnapshotAvailable: true,
        displayHoldMs: request.keepOpenMs, consoleText: 'fixture screen', ...receipt };
    } });
    t.after(() => runner.close());
    const request = { shell: 'cmd', script: 'echo fixture', cwd: ctx.workspaceRoot, visible: true };
    await assert.rejects(runner.run(request), { code: 'HOST_TERMINAL_VISIBLE_UNAVAILABLE' });
    assert.equal(executions, 0);
    currentCapabilities = { ...capabilities, protocolVersion: 2, visibleTerminal: true };
    const complete = await runner.run(request);
    assert.equal(complete.isError, false); assert.equal(complete.value.displayHoldMs, 5000);
    for (const invalid of [{ visibleRequested: false }, { windowObserved: false }, { windowId: '0' },
      { consoleInput: 'pipe' }, { outputCapture: 'stdout' }, { commandCompleted: false },
      { windowProcessId: 0 }, { exitCodeObserved: false }, { outcome: 'unknown' },
      { displayHoldMs: 30001 }, { stdout: 'not a real console' }]) {
      receipt = invalid;
      const unknown = await runner.run(request);
      assert.equal(unknown.status, 'unknown', JSON.stringify(invalid));
      assert.equal(unknown.code, 'HOST_TERMINAL_INVALID_RESULT');
    }
    const before = executions;
    await assert.rejects(runner.run({ ...request, visible: false, keepOpenMs: 1 }), { code: 'HOST_TERMINAL_INVALID_REQUEST' });
    assert.equal(executions, before);
  });

test('late successful host receipt is archived, while interrupted effects preserve unknown status and output', async t => {
  const runner = stub(), controller = new AbortController();
  runner.run = async () => { controller.abort(); return { value: { completed: true, boundary: 'host-terminal', exitCode: 0, stdout: 'done' }, isError: false }; };
  const f = await toolFixture(t, { hostTerminalRunner: runner });
  const complete = await f.run(await f.context('full'), 'terminal.host.run', input, { signal: controller.signal });
  assert.equal(complete.status, 'completed'); assert.ok(complete.resultRef);
  runner.run = async () => ({ value: { completed: false, outcome: 'unknown', stdout: 'partial', cancelled: true },
    isError: true, status: 'unknown', code: 'TOOL_CANCELLED' });
  const partial = await f.run(await f.context('full'), 'terminal.host.run', input);
  assert.equal(partial.status, 'unknown'); assert.ok(partial.resultRef);
  const saved = await f.service.results.read({ conversationId: f.conversationId, requestId: randomUUID() }, partial.resultRef.id, { limit: 16000 });
  assert.match(saved.text, /partial/);
});

test('host tool is directly discoverable for conda and deferred for unrelated tasks in all protocols', () => {
  for (const protocol of ['openai-completions', 'openai-responses', 'anthropic-messages']) {
    const host = new ModelToolCatalog(builtinDescriptors, { protocol, tokenBudget: 16000, message: '调用本机终端执行 conda list' });
    assert.ok(host.selected.some(tool => tool.name === 'terminal.host.run'));
    const visible = new ModelToolCatalog(builtinDescriptors, { protocol, tokenBudget: 8000, message: '跑一个终端可以看到的看看' });
    assert.ok(visible.selected.some(tool => tool.name === 'terminal.host.run'), protocol);
    const ordinary = new ModelToolCatalog(builtinDescriptors, { protocol, tokenBudget: 16000, message: '写一个代码测试' });
    assert.ok(!ordinary.selected.some(tool => tool.name === 'terminal.host.run'));
    ordinary.load(['terminal.host.run']); assert.ok(ordinary.selected.some(tool => tool.name === 'terminal.host.run'));
  }
});

test('an unknown host effect is recorded before the loop stops without another command or model call', async () => {
  let rounds = 0, executions = 0;
  const saved = [];
  const call = { id: 'host-effect', name: 'terminal.host.run', arguments: input };
  await assert.rejects(runToolLoop({ protocol: 'openai-completions', context: {}, messages: [], system: '',
    inputBudgetTokens: 32000, declarations: [], emit: () => {}, saveActivity: async activity => saved.push(activity),
    service: { execute: async () => { executions++; return { content: 'partial host output', isError: true,
      status: 'unknown', code: 'TOOL_TIMED_OUT', resultRef: { id: randomUUID() } }; } },
    requestTurn: async () => { rounds++; return { content: '', reasoning: '', calls: [call], continuation: [{ role: 'assistant',
      content: '', tool_calls: [{ id: call.id, type: 'function', function: { name: call.name, arguments: JSON.stringify(input) } }] }] }; }
  }), { code: 'HOST_TERMINAL_OUTCOME_UNKNOWN' });
  assert.equal(rounds, 1); assert.equal(executions, 1);
  assert.equal(saved.at(-1).status, 'unknown'); assert.ok(saved.at(-1).resultRef);
});

test('actual native host bridge runs CMD/PowerShell in a folderless chat and archives local effects',
  { skip: process.platform !== 'win32', timeout: 45000 }, async t => {
    const runner = new HostTerminalRunner(), f = await toolFixture(t, { hostTerminalRunner: runner });
    const catalog = await f.conversations.catalog(); catalog.Projects[0].FolderPath = null;
    await f.conversations.saveCatalog(catalog);
    const ctx = await f.context('full');
    assert.equal(ctx.hostTerminalCapabilities.available, true, ctx.hostTerminalCapabilities.reason);
    for (const shell of ['cmd', 'powershell']) {
      const result = parsed(await f.run(ctx, 'terminal.host.run', { shell,
        script: shell === 'cmd' ? 'echo host-fixture>host.txt & type host.txt' : "Get-Content -LiteralPath 'host.txt'",
        reason: 'Verify only synthetic temporary host files.' }));
      assert.equal(result.exitCode, 0, result.stderr); assert.equal(result.boundary, 'host-terminal');
      assert.equal(result.activeProcessesAfterExit, 0); assert.match(result.stdout, /host-fixture/);
    }
    assert.match(await readFile(join(ctx.workspaceRoot, 'host.txt'), 'utf8'), /host-fixture/);
    const failed = await f.run(ctx, 'terminal.host.run', { ...input, script: 'exit /b 7' });
    assert.equal(failed.isError, true); assert.equal(failed.status, 'error');
    assert.equal(JSON.parse(failed.content).completed, true); assert.equal(JSON.parse(failed.content).exitCode, 7);
    const shortDirectory = await mkdtemp(join(f.root, 'host-alias-'));
    const short = await runner.run({ shell: 'cmd', script: 'echo alias', cwd: shortDirectory });
    assert.equal(short.isError, false); assert.equal(short.value.cwd, await realpath(shortDirectory));
    runner.invoke = async (_, request) => ({ protocolVersion: 1, boundary: 'host-terminal', completed: true,
      cwd: request.cwd, shell: 'mismatched', exitCode: 0, stdout: 'effect happened', stderr: '', activeProcessesAfterExit: 0 });
    const unverified = await runner.run({ shell: 'cmd', script: 'echo fake', cwd: ctx.workspaceRoot });
    assert.equal(unverified.status, 'unknown'); assert.equal(unverified.value.stdout, 'effect happened');
  });

test('visible command reaches the real native console in a folderless chat and its screen preview is recoverable',
  { skip: process.platform !== 'win32', timeout: 25000 }, async t => {
    const runner = new HostTerminalRunner(), f = await toolFixture(t, { hostTerminalRunner: runner });
    const catalog = await f.conversations.catalog(); catalog.Projects[0].FolderPath = null;
    await f.conversations.saveCatalog(catalog);
    const ctx = await f.context('full');
    assert.equal(ctx.hostTerminalCapabilities.visibleTerminal, true, ctx.hostTerminalCapabilities.reason);
    const call = f.call('terminal.host.run', { ...input, visible: true, keepOpenMs: 200,
      script: 'echo VISIBLE_BROKER_SUCCESS>visible.txt & type visible.txt' });
    const result = await f.service.execute(ctx, call);
    const value = parsed(result);
    assert.equal(result.status, 'completed', result.content); assert.ok(result.resultRef);
    assert.equal(value.windowObserved, true); assert.equal(value.consoleInput, 'console');
    assert.equal(value.outputCapture, 'console-screen'); assert.equal(value.stdout, ''); assert.equal(value.stderr, '');
    assert.match(value.consoleText, /VISIBLE_BROKER_SUCCESS/);
    assert.match(await readFile(join(ctx.workspaceRoot, 'visible.txt'), 'utf8'), /VISIBLE_BROKER_SUCCESS/);
    const saved = await f.service.results.get(ctx, result.resultRef.id);
    assert.equal(saved.structuredContent.consoleText, value.consoleText);
    const history = await f.service.results.modelResult(ctx, result.resultRef,
      { requestId: ctx.requestId, toolCallId: call.id, toolName: 'terminal.host.run' });
    assert.equal(history.structuredContent.consoleText, value.consoleText);
    // The public archive is typed; the model-facing immediate preview must retain actual screen content too.
    // 公开归档保留结果类型；给模型的即时预览也应保留实际屏幕内容。
    assert.match(result.content, /VISIBLE_BROKER_SUCCESS/);
  });

test('real host output streams to the correct tool before completion and still archives exact final streams',
  { skip: process.platform !== 'win32', timeout: 20000 }, async t => {
    const runner = new HostTerminalRunner(), f = await toolFixture(t, { hostTerminalRunner: runner });
    const ctx = await f.context('full'), call = f.call('terminal.host.run', { ...input, shell: 'powershell',
      script: "[Console]::WriteLine('LIVE_FIRST_中文'); Start-Sleep -Milliseconds 250; [Console]::Error.WriteLine('LIVE_ERROR'); [Console]::Write('tail')" });
    const events = []; let completed = false;
    const result = await f.service.execute(ctx, call, { emit: event => {
      assert.equal(completed, false); events.push(event);
    } });
    completed = true;
    const value = parsed(result);
    assert.equal(result.status, 'completed'); assert.ok(events.length >= 2);
    for (const [index, event] of events.entries()) {
      assert.equal(event.type, 'terminal_output'); assert.equal(event.terminal.toolCallId, call.id);
      assert.equal(event.terminal.sequence, index + 1); assert.equal(event.terminal.replace, false);
    }
    const stream = name => events.filter(event => event.terminal.stream === name).map(event => event.terminal.text).join('');
    assert.equal(stream('stdout'), value.stdout); assert.equal(stream('stderr'), value.stderr);
    assert.match(value.stdout, /LIVE_FIRST_中文/); assert.match(value.stderr, /LIVE_ERROR/);
    const archive = await f.service.results.get(ctx, result.resultRef.id);
    assert.equal(archive.structuredContent.stdout, value.stdout);
    assert.equal(archive.structuredContent.stderr, value.stderr);
  });
