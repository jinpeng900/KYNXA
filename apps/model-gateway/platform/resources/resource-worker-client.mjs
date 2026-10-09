import { ResourceBudgetError } from './resource-client.mjs';

const MAX_PENDING_REQUESTS = 32;
const ALLOWED_METHODS = new Set(['acquire', 'renew', 'release', 'report', 'registerExecutor', 'snapshot']);
const abortError = () => Object.assign(new ResourceBudgetError('RESOURCE_CANCELLED', 'Worker resource request was cancelled.'), { name: 'AbortError' });

// Data workers delegate allocations to their owner; they never start a competing monitor.
// 数据工作线程把资源申请交给拥有者，不启动相互竞争的监控或预算器。
export function createResourceWorkerClient(port) {
  let requestSequence = 0, closed = false;
  const pending = new Map();
  const receive = message => {
    if (message?.type !== 'resource_response') return;
    const operation = pending.get(message.resourceRequestId);
    if (!operation) return;
    pending.delete(message.resourceRequestId); operation.cleanup();
    if (message.error) operation.reject(Object.assign(new ResourceBudgetError(message.error.code, message.error.message),
      { name: message.error.name ?? 'ResourceBudgetError' }));
    else {
      if (message.result?.status === 'granted') port.postMessage({ type: 'resource_request', resourceRequestId: ++requestSequence,
        method: 'ackGrant', args: [message.resourceRequestId] });
      operation.resolve(message.result);
    }
  };
  port.on('message', receive);
  function request(method, args, { signal } = {}) {
    if (closed) return Promise.reject(new ResourceBudgetError('RESOURCE_CLOSED', 'Worker resource client is closed.'));
    if (signal?.aborted) return Promise.reject(abortError());
    if (pending.size >= MAX_PENDING_REQUESTS) return Promise.reject(new ResourceBudgetError('RESOURCE_QUEUE_FULL', 'Worker resource queue is full.'));
    const resourceRequestId = ++requestSequence;
    return new Promise((resolveResult, rejectResult) => {
      const cancel = () => {
        port.postMessage({ type: 'resource_request', resourceRequestId: ++requestSequence, method: 'cancel', args: [resourceRequestId] });
        pending.delete(resourceRequestId); cleanup(); rejectResult(abortError());
      };
      const timer = setTimeout(() => {
        port.postMessage({ type: 'resource_request', resourceRequestId: ++requestSequence, method: 'cancel', args: [resourceRequestId] });
        pending.delete(resourceRequestId); cleanup();
        rejectResult(new ResourceBudgetError('RESOURCE_WORKER_TIMEOUT', 'Worker resource operation timed out.'));
      }, Math.min(75_000, 10_000 + (method === 'acquire' ? args[0]?.waitMs ?? 0 : 0)));
      const cleanup = () => { clearTimeout(timer); signal?.removeEventListener('abort', cancel); };
      pending.set(resourceRequestId, { resolve: resolveResult, reject: rejectResult, cleanup, cancel });
      signal?.addEventListener('abort', cancel, { once: true });
      try { port.postMessage({ type: 'resource_request', resourceRequestId, method, args }); }
      catch (error) { pending.delete(resourceRequestId); cleanup(); rejectResult(error); }
    });
  }
  return {
    acquire: (options, context) => request('acquire', [options], context),
    renew: (leaseId, options) => request('renew', [leaseId, { ttlMs: options?.ttlMs }], options),
    release: leaseId => request('release', [leaseId]),
    report: (leaseId, feedback) => request('report', [leaseId, feedback]),
    registerExecutor: (leaseId, executor) => request('registerExecutor', [leaseId, executor]),
    snapshot: options => request('snapshot', [], options),
    close() {
      if (closed) return;
      for (const operation of [...pending.values()]) operation.cancel();
      closed = true; port.removeListener('message', receive);
    },
  };
}

export function attachResourceWorkerBridge(worker, service, { context = {} } = {}) {
  const operations = new Map(), leases = new Set(), executors = new Map(), delivered = new Map();
  let exited = false;
  const receive = async message => {
    if (message?.type !== 'resource_request' || exited) return;
    const { resourceRequestId, method, args } = message;
    if (!Number.isSafeInteger(resourceRequestId) || resourceRequestId < 1 || !Array.isArray(args)) return;
    if (method === 'ackGrant') { delivered.delete(args[0]); return; }
    if (method === 'cancel') {
      operations.get(args[0])?.abort();
      const leaseId = delivered.get(args[0]);
      if (leaseId) { delivered.delete(args[0]); try { await service.release(leaseId); leases.delete(leaseId); }
        catch { /* Unconfirmed release stays fenced until exit cleanup. 未确认释放的预约继续隔离，等待退出清理。 */ } }
      return;
    }
    let result, error;
    const controller = new AbortController();
    try {
      if (!ALLOWED_METHODS.has(method) || operations.has(resourceRequestId) || operations.size >= MAX_PENDING_REQUESTS)
        throw new ResourceBudgetError('RESOURCE_QUEUE_FULL', 'Owner resource bridge rejected the request.');
      if (!['acquire', 'snapshot'].includes(method) && !leases.has(args[0]))
        throw new ResourceBudgetError('RESOURCE_LEASE_UNKNOWN', 'Worker cannot modify another owner reservation.');
      operations.set(resourceRequestId, controller);
      if (method === 'acquire') {
        const options = { ...args[0], kind: context.kind ?? args[0]?.kind ?? 'background',
          workspaceId: context.workspaceId ?? args[0]?.workspaceId ?? '',
          taskId: `${context.taskIdPrefix ?? 'worker'}:${args[0]?.taskId ?? resourceRequestId}`.slice(0, 128) };
        result = await service.acquire(options, { signal: controller.signal });
        if (result?.status === 'granted') leases.add(result.leaseId);
      } else if (method === 'snapshot') result = await service.snapshot({ signal: controller.signal });
      else if (method === 'registerExecutor') result = await service.registerExecutor(args[0],
        { processId: args[1]?.processId ?? process.pid, ...(args[1]?.startTimeMs !== undefined ? { startTimeMs: args[1].startTimeMs } : {}) });
      else result = await service[method](...args);
      if (method === 'registerExecutor' && result?.status === 'registered') executors.set(args[0], result.executor);
      if (method === 'release') { leases.delete(args[0]); executors.delete(args[0]);
        for (const [id, leaseId] of delivered) if (leaseId === args[0]) delivered.delete(id); }
      if ((controller.signal.aborted || exited) && result?.status === 'granted') {
        await service.release(result.leaseId); leases.delete(result.leaseId); result = undefined;
        throw abortError();
      }
    } catch (failure) {
      error = { name: failure.name, code: failure.code ?? 'RESOURCE_WORKER_FAILED', message: 'Worker resource operation failed.' };
    } finally { operations.delete(resourceRequestId); }
    if (!exited) { try {
      if (result?.status === 'granted') delivered.set(resourceRequestId, result.leaseId);
      worker.postMessage({ type: 'resource_response', resourceRequestId, result, error });
    } catch { /* Exit cleanup owns remaining leases. 退出清理负责仍保留的预约。 */ } }
  };
  const exit = () => {
    exited = true; worker.removeListener('message', receive);
    for (const controller of operations.values()) controller.abort();
    // Thread exit does not prove that its forked helper has exited; uncertain child memory stays fenced.
    // 线程退出不能证明其 fork 子进程已经退出；子进程状态不明时继续隔离保留内存。
    Promise.allSettled([...leases].map(async leaseId => {
      const executor = executors.get(leaseId);
      if (executor && executor.processId !== process.pid) {
        for (let attempt = 0; attempt < 25; attempt++) {
          const snapshot = await service.snapshot();
          if (snapshot.mode !== 'rust') return;
          const current = snapshot.executors?.processes?.find(entry => entry.processId === executor.processId);
          if (!current || current.state !== 'running' || current.startTimeMs !== executor.startTimeMs) break;
          if (attempt === 24) return;
          await new Promise(resolve => setTimeout(resolve, 200));
        }
      }
      await service.release(leaseId); leases.delete(leaseId); executors.delete(leaseId);
    }));
  };
  worker.on('message', receive); worker.once('exit', exit);
  return { cancelPending() { for (const controller of operations.values()) controller.abort(); } };
}
