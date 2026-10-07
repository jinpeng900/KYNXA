import { EventEmitter } from 'node:events';

const PARENT_DISCONNECT_TIMEOUT_MS = 30_000;

// Bootstrap over IPC before loading any model; buffering preserves the first queued request.
// 先通过 IPC 接收启动配置，再加载模型；缓冲启动期间的消息，保留首个排队请求。
export async function openNativeInferencePort() {
  if (typeof process.send !== 'function') throw new Error('Native inference requires an owned IPC process.');
  let resolveConfiguration, isInitialized = false, isClosed = false, disconnectTimer;
  const configuration = new Promise(resolve => { resolveConfiguration = resolve; });
  const bufferedMessages = [];
  const port = new EventEmitter();
  const deliver = message => {
    if (port.listenerCount('message')) port.emit('message', message);
    else bufferedMessages.push(message);
  };
  const receiveMessage = message => {
    if (!isInitialized && message?.type === 'initialize') {
      isInitialized = true;
      resolveConfiguration(message.workerData);
      return;
    }
    if (isInitialized) deliver(message);
  };
  const disconnect = () => {
    if (isClosed) return;
    // Parent loss also retires the model; a bounded watchdog prevents orphaned native processes.
    // 父进程丢失时同样退役模型；有界看门狗防止原生调用挂起后留下孤儿进程。
    disconnectTimer = setTimeout(() => process.exit(1), PARENT_DISCONNECT_TIMEOUT_MS);
    disconnectTimer.unref();
    if (!isInitialized) process.exit(1);
    deliver({ type: 'close' });
  };
  process.on('message', receiveMessage);
  process.on('disconnect', disconnect);
  port.on('newListener', event => {
    if (event !== 'message') return;
    queueMicrotask(() => {
      for (const message of bufferedMessages.splice(0)) port.emit('message', message);
    });
  });
  port.postMessage = message => {
    if (process.connected && !isClosed) process.send(message, () => {});
  };
  port.close = () => {
    isClosed = true;
    clearTimeout(disconnectTimer);
    process.off('message', receiveMessage);
    process.off('disconnect', disconnect);
    if (process.connected) process.disconnect();
  };
  return { parentPort: port, workerData: await configuration };
}
