import assert from 'node:assert/strict';
import { test } from 'node:test';
import { prepareSourcePipeline, IndexPublicationStage } from '../orchestration/retrieval/index-pipeline.mjs';

const gate = () => {
  let release;
  const promise = new Promise(resolve => { release = resolve; });
  return { promise, release };
};

test('bounded preparation overlaps consumption while preserving source order and file-level failures', async () => {
  const blocked = gate(), consuming = gate();
  let running = 0, peak = 0, sawOverlap = false;
  const pipeline = prepareSourcePipeline([0, 1, 2, 3], async input => {
    running++; peak = Math.max(peak, running);
    if (input === 1) await blocked.promise;
    if (input === 2) { await consuming.promise; sawOverlap = true; }
    running--;
    if (input === 3) throw Object.assign(new Error('Bad source'), { code: 'SOURCE_PARSE_FAILED' });
    return input * 2;
  });
  const first = await pipeline.next();
  assert.equal(first.value.input, 0);
  consuming.release(); blocked.release();
  const rest = []; for await (const result of pipeline) rest.push(result);
  assert.deepEqual(rest.map(result => result.input), [1, 2, 3]);
  assert.equal(rest[1].value, 4); assert.equal(rest[2].error.code, 'SOURCE_PARSE_FAILED');
  assert.equal(sawOverlap, true); assert.ok(peak <= 2);
});

test('cancellation stops admission and drains accepted native preparation before returning', async () => {
  const started = gate(), blocked = gate(), controller = new AbortController();
  const admitted = [];
  const pipeline = prepareSourcePipeline([0, 1, 2], async input => {
    admitted.push(input);
    if (input === 1) { started.release(); await blocked.promise; }
    return input;
  }, { concurrency: 2, signal: controller.signal });
  await pipeline.next(); await started.promise;
  controller.abort();
  let stopped = false;
  const returning = pipeline.return().then(() => { stopped = true; });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(stopped, false, 'native lifetime remains owned until it settles');
  blocked.release(); await returning;
  assert.equal(stopped, true); assert.ok(admitted.length <= 3);
});

test('publication acknowledges each commit before admitting the next operation and propagates failures', async () => {
  const committed = gate(), events = [], stage = new IndexPublicationStage();
  await stage.submit(async () => { events.push('start-one'); await committed.promise; events.push('commit-one'); return 1; });
  const next = stage.submit(async () => { events.push('start-two'); throw new Error('Publish failed'); });
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(events, ['start-one']);
  committed.release(); await next;
  await assert.rejects(stage.drain(), /Publish failed/);
  assert.deepEqual(events, ['start-one', 'commit-one', 'start-two']);
  assert.equal(await stage.drain(), undefined, 'a failed publication is not replayed');
});
