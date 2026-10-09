import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Worker } from 'node:worker_threads';
import { once } from 'node:events';
import { ResourceBudgetService } from '../platform/resources/resource-client.mjs';
import { attachResourceWorkerBridge } from '../platform/resources/resource-worker-client.mjs';

const hardware = () => ({ cpu: { logicalCores: 8, usagePercent: 10 },
  memory: { totalBytes: 16 * 2 ** 30, availableBytes: 8 * 2 ** 30 }, gpu: { state: 'unknown', availableMemoryBytes: null } });
const workerClientUrl = new URL('../platform/resources/resource-worker-client.mjs', import.meta.url).href;

test('a real data worker delegates to one authority and memory is released only after its confirmed exit', async () => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-resource-worker-'));
  const script = join(root, 'worker.mjs');
  await writeFile(script, `import {parentPort} from 'node:worker_threads';
import {createResourceWorkerClient} from ${JSON.stringify(workerClientUrl)};
const resources=createResourceWorkerClient(parentPort);
const lease=await resources.acquire({taskId:'ann',cpuThreads:2,memoryBytes:16777216});
const registration=await resources.registerExecutor(lease.leaseId,{processId:process.pid});
const feedback=await resources.report(lease.leaseId,{throughputPerSecond:10,latencyMs:1,queueDepth:1});
parentPort.postMessage({type:'ready',lease,registration,feedback});
parentPort.on('message',message=>{if(message.type==='stop'){resources.close();parentPort.close();}});`);
  const service = new ResourceBudgetService({ executablePath: null, sampler: hardware });
  const worker = new Worker(script);
  attachResourceWorkerBridge(worker, service, { context: { taskIdPrefix: 'index-worker', workspaceId: 'fixture' } });
  try {
    const message = await new Promise((resolve, reject) => { worker.on('message', message => { if (message.type === 'ready') resolve(message); }); worker.once('error', reject); });
    assert.equal(message.lease.status, 'granted'); assert.equal(message.registration.status, 'registered');
    assert.equal(message.feedback.status, 'reported');
    assert.equal((await service.snapshot()).budget.reservedMemoryBytes, 16777216);
    const exited = once(worker, 'exit'); worker.postMessage({ type: 'stop' }); await exited;
    await new Promise(resolve => setTimeout(resolve, 10));
    assert.equal((await service.snapshot()).activeLeases, 0);
  } finally { await worker.terminate(); await service.close(); await rm(root, { recursive: true, force: true }); }
});

test('worker cancellation removes waiting admission without releasing another worker reservation', async () => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-resource-worker-'));
  const script = join(root, 'worker.mjs');
  await writeFile(script, `import {parentPort} from 'node:worker_threads';
import {createResourceWorkerClient} from ${JSON.stringify(workerClientUrl)};
const resources=createResourceWorkerClient(parentPort);const controller=new AbortController();
const request=resources.acquire({taskId:'cancelled',cpuThreads:4,waitMs:5000},{signal:controller.signal});
setTimeout(()=>controller.abort(),20);
try{await request;parentPort.postMessage({type:'unexpected-grant'});}catch(error){parentPort.postMessage({type:'cancelled',name:error.name});}
resources.close();parentPort.close();`);
  const service = new ResourceBudgetService({ executablePath: null, sampler: hardware });
  const owner = await service.acquire({ taskId: 'other-owner', cpuThreads: 4 });
  const worker = new Worker(script); attachResourceWorkerBridge(worker, service);
  try {
    const outcome = await new Promise((resolve, reject) => { worker.on('message', message => {
      if (['cancelled', 'unexpected-grant'].includes(message.type)) resolve(message);
    }); worker.once('error', reject); });
    assert.equal(outcome.name, 'AbortError');
    await new Promise(resolve => setTimeout(resolve, 10));
    assert.equal((await service.snapshot()).queuedRequests, 0);
    assert.equal((await service.snapshot()).budget.reservedCpuThreads, 4);
    await service.release(owner.leaseId);
    assert.equal((await service.snapshot()).activeLeases, 0);
  } finally { await worker.terminate(); await service.close(); await rm(root, { recursive: true, force: true }); }
});

test('cancellation between owner approval and worker delivery releases an unused grant before worker exit', async () => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-resource-worker-'));
  const script = join(root, 'worker.mjs');
  await writeFile(script, `import {parentPort} from 'node:worker_threads';
import {createResourceWorkerClient} from ${JSON.stringify(workerClientUrl)};
const resources=createResourceWorkerClient(parentPort);const controller=new AbortController();
const request=resources.acquire({taskId:'delivery-race',cpuThreads:1},{signal:controller.signal});
setTimeout(()=>controller.abort(),20);
try{await request;}catch(error){parentPort.postMessage({type:'cancelled',name:error.name});}
parentPort.on('message',message=>{if(message.type==='stop'){resources.close();parentPort.close();}});`);
  const service = new ResourceBudgetService({ executablePath: null, sampler: hardware });
  const worker = new Worker(script), post = worker.postMessage.bind(worker);
  worker.postMessage = message => {
    if (message.type === 'resource_response' && message.result?.status === 'granted') setTimeout(() => post(message), 60);
    else post(message);
  };
  attachResourceWorkerBridge(worker, service);
  try {
    const outcome = await new Promise((resolve, reject) => { worker.on('message', message => { if (message.type === 'cancelled') resolve(message); }); worker.once('error', reject); });
    assert.equal(outcome.name, 'AbortError');
    await new Promise(resolve => setTimeout(resolve, 10));
    assert.equal((await service.snapshot()).activeLeases, 0);
    const exited = once(worker, 'exit'); post({ type: 'stop' }); await exited;
  } finally { await worker.terminate(); await service.close(); await rm(root, { recursive: true, force: true }); }
});
