import { fork } from 'node:child_process';
import { EventEmitter } from 'node:events';

const TERMINATION_TIMEOUT_MS = 5000;

// Each native model owns a Node process, so ONNX process-global state and crashes stay isolated.
// 每个原生模型独占 Node 进程，隔离 ONNX 进程级状态及崩溃；兼容已有 worker 消息接口。
class NativeInferenceProcess extends EventEmitter {
  #child;
  #exit;
  #exited = false;
  #termination;

  constructor(moduleUrl, { workerData, execArgv = [] } = {}) {
    super();
    this.#child = fork(moduleUrl, [], {
      execArgv, windowsHide: true, serialization: 'advanced',
      stdio: ['ignore', 'ignore', 'ignore', 'ipc'],
    });
    this.#exit = new Promise(resolveExit => {
      this.#child.once('close', (exitCode, signal) => {
        this.#exited = true;
        resolveExit({ exitCode, signal });
        this.emit('exit', exitCode, signal);
      });
    });
    this.#child.on('message', message => this.emit('message', message));
    this.#child.on('error', error => this.emit('error', error));
    this.postMessage({ type: 'initialize', workerData });
  }

  get pid() { return this.#child.pid; }

  postMessage(message) {
    if (this.#exited || !this.#child.connected) throw new Error('Native inference process is disconnected.');
    this.#child.send(message, error => {
      if (error && !this.#exited) this.emit('error', error);
    });
  }

  ref() {
    this.#child.ref();
    this.#child.channel?.ref();
  }

  unref() {
    this.#child.unref();
    this.#child.channel?.unref();
  }

  terminate() {
    if (this.#termination) return this.#termination;
    // Killing this owned process cannot tear down a native session inside the gateway.
    // 强制结束仅针对本服务拥有的进程，不会拆毁网关进程中的原生会话。
    this.ref();
    let timer;
    this.#termination = Promise.race([this.#exit, new Promise((resolveExit, rejectExit) => {
      timer = setTimeout(() => rejectExit(new Error('Native inference process could not be reaped.')), TERMINATION_TIMEOUT_MS);
      if (!this.#exited) this.#child.kill('SIGKILL');
    })]).finally(() => clearTimeout(timer));
    return this.#termination;
  }
}

export function createNativeInferenceProcess(moduleUrl, options) {
  return new NativeInferenceProcess(moduleUrl, options);
}
