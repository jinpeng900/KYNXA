import { parentPort, workerData } from 'node:worker_threads';
import { DatabaseSync } from 'node:sqlite';
import { join } from 'node:path';

const database = new DatabaseSync(join(workerData.root, 'deadline-probe.sqlite'));
database.exec('CREATE TABLE IF NOT EXISTS markers(value TEXT);');
const guard = (id, canRetire) => parentPort.postMessage({ type: 'operation_started', id, canRetire });
const cancelled = flag => {
  if (Atomics.load(flag, 0)) throw Object.assign(new Error('Fixture acknowledged cancellation.'), {
    code: Atomics.load(flag, 0) === 2 ? 'RETRIEVAL_SEARCH_TIMEOUT' : 'ABORT_ERR', statusCode: 504 });
};

parentPort.on('message', message => {
  if (message?.type === 'resource_response') return;
  const flag = new Int32Array(message.cancelBuffer);
  try {
    cancelled(flag);
    if (message.method === 'search') {
      guard(message.id, true);
      // This native SQL has no JS callback or progress handler; the owner must really terminate it when safe.
      // 此原生 SQL 没有 JS 回调或 progress handler，拥有者必须在安全时真正终止线程，不能只提前返回。
      database.prepare(`WITH RECURSIVE probe(value) AS (SELECT 1 UNION ALL SELECT value+1 FROM probe WHERE value<4000000)
        SELECT sum(value) FROM probe`).get();
      cancelled(flag);
      parentPort.postMessage({ id: message.id, result: { items: [] } });
    } else if (message.method === 'upsertSources') {
      guard(message.id, false);
      database.exec('BEGIN; INSERT INTO markers VALUES (\'committed-once\');');
      parentPort.postMessage({ type: 'probe_transaction_open' });
      // Deliberately hold a real uncommitted transaction so queued search expiry cannot kill it.
      // 故意保持真实未提交事务，用来验证排队搜索过期不会截停写入。
      Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 300);
      database.exec('COMMIT');
      parentPort.postMessage({ id: message.id, result: { committed: true } });
    } else if (message.method === 'close') {
      guard(message.id, false); database.close();
      parentPort.postMessage({ id: message.id, result: { closed: true } });
    } else {
      guard(message.id, true);
      parentPort.postMessage({ id: message.id, result: { ready: true } });
    }
  } catch (error) {
    parentPort.postMessage({ id: message.id, error: { name: error.name, message: error.message,
      code: error.code ?? 'PROBE_FAILED', statusCode: error.statusCode ?? 500 } });
  }
});
