import assert from 'node:assert/strict';
import { test } from 'node:test';
import { runResourceTask } from '../platform/resources/resource-task.mjs';
import { runToolLoop } from '../orchestration/tool-loop.mjs';

function resourceFixture(responses) {
  const requests = [], released = [];
  return { requests, released,
    async acquire(request) {
      requests.push(request);
      const response = responses.shift();
      if (response instanceof Error) throw response;
      assert.ok(response, 'admission retries are bounded');
      return response;
    },
    async renew() { return { status: 'renewed' }; },
    async release(id) { released.push(id); return { status: 'released' }; }
  };
}
const denied = () => ({ status: 'denied', reason: 'RESOURCE_WAIT_TIMEOUT', mode: 'fixture' });
const granted = () => ({ status: 'granted', leaseId: 'owned-foreground' });

test('confirmed idle release retries only admission and preserves the requested resource budget', async () => {
  const resources = resourceFixture([denied(), granted()]);
  let operations = 0, recoveries = 0;
  const value = await runResourceTask(resources, { taskId: 'foreground', memoryBytes: 16 * 1024 ** 2 }, lease => {
    operations++;
    assert.equal(lease.admissionRecovery.initialReason, 'RESOURCE_WAIT_TIMEOUT');
    assert.equal(lease.admissionRecovery.granted, true);
    return 'executed once';
  }, { onCapacityUnavailable: async options => {
    recoveries++;
    assert.equal(options.reason, 'memory-pressure');
    assert.equal(options.minimumIdleMs, 0);
    return { released: true };
  } });
  assert.equal(value, 'executed once'); assert.equal(operations, 1); assert.equal(recoveries, 1);
  assert.deepEqual(resources.requests[0], resources.requests[1]);
  assert.deepEqual(resources.released, ['owned-foreground']);
});

test('busy inference and a second denial do not loop or execute without a lease', async () => {
  for (const idleReleased of [false, true]) {
    const resources = resourceFixture([denied(), denied()]);
    let recoveries = 0;
    await assert.rejects(runResourceTask(resources, {}, () => assert.fail('not admitted'), {
      onCapacityUnavailable: async () => { recoveries++; return { released: idleReleased }; }
    }), error => {
      assert.equal(error.code, 'RESOURCE_WAIT_TIMEOUT');
      if (idleReleased) assert.equal(error.details.admissionRecovery.granted, false);
      return true;
    });
    assert.equal(recoveries, 1); assert.equal(resources.requests.length, idleReleased ? 2 : 1);
    assert.deepEqual(resources.released, []);
  }
});

test('unknown allocation transport failures and policy denials are never replayed', async () => {
  for (const response of [Object.assign(new Error('allocation reply lost'), { code: 'RESOURCE_TRANSPORT_FAILED' }),
    { status: 'denied', reason: 'RESOURCE_INVALID_REQUEST' }, { status: 'denied', reason: 'RESOURCE_GPU_UNKNOWN' }]) {
    const resources = resourceFixture([response]);
    await assert.rejects(runResourceTask(resources, {}, () => assert.fail('not admitted'), {
      onCapacityUnavailable: () => assert.fail('not a recoverable capacity denial')
    }));
    assert.equal(resources.requests.length, 1);
  }
});

test('an operation failure never triggers admission recovery or repeats its side effect', async () => {
  const resources = resourceFixture([granted()]);
  let effects = 0;
  await assert.rejects(runResourceTask(resources, {}, () => {
    effects++;
    throw Object.assign(new Error('operation outcome unknown'), { code: 'RESOURCE_WAIT_TIMEOUT' });
  }, { onCapacityUnavailable: () => assert.fail('operation has already started') }), { code: 'RESOURCE_WAIT_TIMEOUT' });
  assert.equal(effects, 1); assert.equal(resources.requests.length, 1);
  assert.deepEqual(resources.released, ['owned-foreground']);
});

test('cancellation and uncertain inference retirement prevent renewed admission', async () => {
  for (const cancel of [false, true]) {
    const controller = new AbortController(), resources = resourceFixture([denied()]);
    await assert.rejects(runResourceTask(resources, {}, () => assert.fail('not admitted'), {
      signal: controller.signal, onCapacityUnavailable: async () => {
        if (!cancel) throw Object.assign(new Error('worker has not exited'), { code: 'EMBEDDING_CLOSE_TIMEOUT' });
        controller.abort(); return { released: true };
      }
    }), cancel ? { name: 'AbortError' } : { code: 'EMBEDDING_CLOSE_TIMEOUT' });
    assert.equal(resources.requests.length, 1);
  }
});

test('the production tool loop reclaims idle inference before one model dispatch, without spending an extra round', async () => {
  const resources = resourceFixture([denied(), granted()]);
  let modelRequests = 0, idleReleases = 0;
  const states = [];
  const result = await runToolLoop({ protocol: 'openai-completions',
    messages: [{ role: 'user', content: 'Read the current repository.' }], system: '', declarations: [],
    inputBudgetTokens: 32000, context: { conversationId: 'fixture', requestId: 'fixture-turn' },
    service: { resources, retrieval: { embeddings: { async releaseIdleResources() {
      idleReleases++; return { released: true };
    } } } },
    emit: () => {}, saveActivity: async () => {}, saveRunState: state => states.push(state),
    requestTurn: async () => { modelRequests++; return { content: 'The current source is available.',
      reasoning: '', calls: [], continuation: [] }; }
  });
  assert.equal(result.content, 'The current source is available.');
  assert.equal(modelRequests, 1); assert.equal(idleReleases, 1);
  assert.equal(states.at(-1).rounds, 1);
  assert.equal(resources.requests.length, 2);
});
