import assert from 'node:assert/strict';
import { test } from 'node:test';
import { HostTerminalJobs } from '../tools/host-terminal-jobs.mjs';

function fixture() {
  const pending = [];
  const runner = { run: async (request, signal, onOutput, onStarted) => new Promise(resolve => {
    const item = { request, signal, onOutput, resolve };
    pending.push(item);
    signal.addEventListener('abort', () => resolve({ value: { completed: false, outcome: 'unknown', cancelled: true,
      activeProcessesAfterExit: 0 }, isError: true, status: 'unknown', code: 'TOOL_CANCELLED' }), { once: true });
    onStarted({ processId: pending.length + 100 });
  }) };
  return { pending, service: new HostTerminalJobs({ runner, maximumRunning: 1 }) };
}
const context = { conversationId: 'synthetic-chat' };
const request = { shell: 'cmd', script: 'fixture', cwd: 'synthetic-workspace' };

test('background job outlives its completed model request and exposes paged output with terminal receipt', async t => {
  const f = fixture(), controller = new AbortController();
  t.after(() => f.service.close());
  const started = await f.service.start(context, request, controller.signal);
  assert.equal(started.status, 'running'); assert.equal(started.running, true);
  assert.equal(f.pending[0].request.backgroundJob, true); assert.equal(f.pending[0].request.timeoutMs, 1800000);
  controller.abort(); assert.equal(f.pending[0].signal.aborted, false);
  f.pending[0].onOutput({ stream: 'stdout', text: 'abcdefgh' });
  const first = f.service.read(context, { jobId: started.jobId, limit: 3 });
  assert.equal(first.output, 'abc'); assert.equal(first.nextOffset, 3); assert.equal(first.hasMore, true);
  assert.equal(f.service.read(context, { jobId: started.jobId, offset: 3 }).output, 'defgh');
  f.pending[0].resolve({ value: { completed: true, exitCode: 0, stdout: 'abcdefgh', stderr: '', activeProcessesAfterExit: 0 }, isError: false });
  await f.service.jobs.get(started.jobId).completion;
  const completed = f.service.read(context, { jobId: started.jobId });
  assert.equal(completed.status, 'completed'); assert.equal(completed.running, false);
  assert.equal(completed.receipt.exitCode, 0); assert.equal(completed.receipt.stdout, undefined);
});

test('jobs preserve chat ownership, capacity, cancellation receipts and gateway close cleanup', async () => {
  const f = fixture(), started = await f.service.start(context, request);
  assert.throws(() => f.service.read({ conversationId: 'other-chat' }, { jobId: started.jobId }), { code: 'HOST_TERMINAL_JOB_NOT_FOUND' });
  await assert.rejects(f.service.stop({ conversationId: 'other-chat' }, { jobId: started.jobId }), { code: 'HOST_TERMINAL_JOB_NOT_FOUND' });
  await assert.rejects(f.service.start(context, request), { code: 'HOST_TERMINAL_CAPACITY' });
  const stopped = await f.service.stop(context, { jobId: started.jobId });
  assert.equal(stopped.status, 'unknown'); assert.equal(stopped.receipt.cancelled, true);
  assert.equal(stopped.receipt.activeProcessesAfterExit, 0);
  const next = await f.service.start(context, request);
  await f.service.close();
  assert.equal(f.pending[1].signal.aborted, true);
  assert.equal(f.service.read(context, { jobId: next.jobId }).running, false);
  await assert.rejects(f.service.start(context, request), { code: 'HOST_TERMINAL_UNAVAILABLE' });
});

test('startup failure is never reported as a running task and large output has explicit cursor loss', async t => {
  const failure = new HostTerminalJobs({ runner: { run: async () => { throw Object.assign(new Error('Fixture unavailable'), { code: 'HOST_TERMINAL_UNAVAILABLE' }); } } });
  t.after(() => failure.close());
  await assert.rejects(failure.start(context, request), { code: 'HOST_TERMINAL_UNAVAILABLE' });
  const f = fixture(); t.after(() => f.service.close());
  const started = await f.service.start(context, request);
  f.pending[0].onOutput({ stream: 'stdout', text: 'x'.repeat(262200) });
  const value = f.service.read(context, { jobId: started.jobId, offset: 0, limit: 5 });
  assert.equal(value.truncated, true); assert.equal(value.earliestOffset, 56); assert.equal(value.offset, 56);
  assert.equal(value.nextOffset, 61); assert.equal(value.totalCharacters, 262200);
});

test('background output pages preserve supplementary characters and reject a split cursor', async t => {
  const f = fixture(); t.after(() => f.service.close());
  const started = await f.service.start(context, request);
  f.pending[0].onOutput({ stream: 'stdout', text: '前😀后' });
  const first = f.service.read(context, { jobId: started.jobId, limit: 2 });
  assert.equal(first.output, '前'); assert.equal(first.nextOffset, 1);
  const next = f.service.read(context, { jobId: started.jobId, offset: first.nextOffset, limit: 1 });
  assert.equal(next.output, '😀'); assert.equal(next.nextOffset, 3);
  assert.throws(() => f.service.read(context, { jobId: started.jobId, offset: 2 }), { code: 'HOST_TERMINAL_INVALID_OFFSET' });
});

test('cancelling a startup acknowledgement stops its owned runner without reporting a started job', async () => {
  const runner = { run: async (_, signal) => new Promise(resolve => signal.addEventListener('abort', () => resolve({
    value: { cancelled: true, completed: false, activeProcessesAfterExit: 0 }, status: 'unknown', isError: true
  }), { once: true })) };
  const service = new HostTerminalJobs({ runner }), controller = new AbortController();
  const started = service.start(context, request, controller.signal);
  await Promise.resolve(); controller.abort();
  await assert.rejects(started, { name: 'AbortError' });
  await service.close();
  assert.equal([...service.jobs.values()][0].status, 'unknown');
  assert.equal([...service.jobs.values()][0].processId, undefined);
});

test('missing startup acknowledgement never invites replay of an already completed or uncertain command', async t => {
  for (const result of [{ value: { completed: true, exitCode: 0 }, isError: false },
    { value: { completed: false, outcome: 'unknown' }, status: 'unknown', isError: true }]) {
    const service = new HostTerminalJobs({ runner: { run: async () => result } });
    t.after(() => service.close());
    let failure;
    try { await service.start(context, request); } catch (error) { failure = error; }
    assert.equal(failure.code, 'HOST_TERMINAL_INVALID_RESULT'); assert.equal(failure.outcomeUnknown, true);
    assert.ok(failure.jobId); assert.match(failure.message, new RegExp(failure.jobId));
    assert.equal(service.read(context, { jobId: failure.jobId }).running, false);
  }
});
