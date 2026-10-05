import { spawn } from 'node:child_process';
import { isAbsolute, resolve } from 'node:path';
import { StringDecoder } from 'node:string_decoder';
import { realpath } from 'node:fs/promises';
import { findNativeToolHost } from './tool-host-path.mjs';
import { inspectLocalPath, toolFailure } from '../platform/tool-paths.mjs';

const boundary = 'host-terminal';
const maxTransportBytes = 2 * 1024 * 1024;
const validEnvelope = value => [1, 2].includes(value?.protocolVersion) && value.boundary === boundary;

function verifiesVisibleConsole(value, keepOpenMs) {
  return value.protocolVersion === 2 && value.visibleRequested === true && value.windowObserved === true &&
    typeof value.windowId === 'string' && /^\d+$/.test(value.windowId) && value.windowId !== '0' &&
    Number.isSafeInteger(value.windowProcessId) && value.windowProcessId > 0 &&
    value.consoleInput === 'console' && value.consoleOutput === 'console' && value.outputCapture === 'console-screen' &&
    value.commandCompleted === true && value.exitCodeObserved === true && value.outcome === 'completed' &&
    typeof value.consoleText === 'string' && typeof value.consoleSnapshotAvailable === 'boolean' &&
    Number.isSafeInteger(value.displayHoldMs) && value.displayHoldMs >= 0 && value.displayHoldMs <= keepOpenMs &&
    value.stdout === '' && value.stderr === '';
}

/**
 * Explicit host execution with retained receipts; never a fallback for the sandbox terminal.
 * 显式宿主执行保留回执，绝不作为沙箱终端失败后的隐式回退。
 */
export class HostTerminalRunner {
  constructor({ toolHostPath, invoke } = {}) {
    this.toolHostPath = toolHostPath;
    this.invoke = invoke ?? invokeHostTerminal;
    this.shutdown = new AbortController();
    this.active = new Set();
    this.pendingCount = 0;
  }

  async _invoke(request, signal, timeoutMs, onOutput) {
    this.shutdown.signal.throwIfAborted(); signal?.throwIfAborted();
    if (this.pendingCount >= 8) throw toolFailure('本机终端并发已达上限，请稍后重试。', 'HOST_TERMINAL_CAPACITY', 429);
    this.pendingCount++;
    let operation;
    try {
      const host = await findNativeToolHost(this.toolHostPath, 'HOST_TERMINAL_UNAVAILABLE');
      this.shutdown.signal.throwIfAborted(); signal?.throwIfAborted();
      operation = this.invoke(host, request, signal, timeoutMs, onOutput);
      this.active.add(operation);
      return await operation;
    } finally { this.active.delete(operation); this.pendingCount--; }
  }

  async capabilities() {
    try {
      const value = await this._invoke({ operation: 'host_terminal_capabilities' }, this.shutdown.signal, 10000);
      if (!validEnvelope(value) || value.available !== true || value.sandbox !== false ||
          value.processTreeBounded !== true || !Array.isArray(value.shells) || !value.shells.length ||
          value.shells.some(shell => !['cmd', 'powershell'].includes(shell)))
        throw toolFailure('原生助手没有可用的本机终端能力。', 'HOST_TERMINAL_UNAVAILABLE', 503);
      return value;
    } catch (error) {
      return { protocolVersion: 1, boundary, available: false, shells: [], sandbox: false, reason: error.message };
    }
  }

  async run({ shell, script, cwd, timeoutMs = 30000, visible = false, keepOpenMs }, signal, onOutput) {
    if (!['cmd', 'powershell'].includes(shell) || typeof script !== 'string' || !script.trim() ||
        script.length > 16384 || script.includes('\0') || !Number.isSafeInteger(timeoutMs) || timeoutMs < 100 || timeoutMs > 120000)
      throw toolFailure('本机终端的命令或超时参数无效。', 'HOST_TERMINAL_INVALID_REQUEST');
    if (typeof visible !== 'boolean' || (keepOpenMs !== undefined && (!visible ||
        !Number.isSafeInteger(keepOpenMs) || keepOpenMs < 0 || keepOpenMs > 30000)))
      throw toolFailure('本机终端窗口参数无效。', 'HOST_TERMINAL_INVALID_REQUEST');
    const holdMs = keepOpenMs ?? (visible ? 5000 : 0);
    if (typeof cwd !== 'string' || !isAbsolute(cwd) || /^\\\\/.test(cwd) || /[\0\r\n]/.test(cwd))
      throw toolFailure('本机终端需要有效的绝对本地目录。', 'HOST_TERMINAL_INVALID_WORKSPACE');
    cwd = resolve(cwd);
    if (!(await inspectLocalPath(cwd)).isDirectory())
      throw toolFailure('本机终端工作目录不存在。', 'HOST_TERMINAL_INVALID_WORKSPACE');
    // .NET expands Windows short paths in its receipt; send the canonical identity too.
    // .NET 会在回执中展开 Windows 短路径，因此也发送规范化身份用于核验。
    cwd = await realpath(cwd);
    await inspectLocalPath(cwd);
    const combined = signal ? AbortSignal.any([signal, this.shutdown.signal]) : this.shutdown.signal;
    combined.throwIfAborted();
    // Older helpers ignore unknown fields. Check before dispatch so a visible request cannot execute hidden.
    // 旧助手可能忽略未知字段，派发前先检查，防止可见窗口请求被隐式执行为隐藏模式。
    if (visible) {
      const current = await this.capabilities();
      combined.throwIfAborted();
      if (!current.available || current.protocolVersion !== 2 || current.visibleTerminal !== true)
        throw toolFailure('当前原生助手不支持可见终端，请更新后重试。', 'HOST_TERMINAL_VISIBLE_UNAVAILABLE', 503);
    }
    // A distinct operation also fails closed if the helper is replaced after discovery: old helpers reject it.
    // 使用独立操作名，即使发现能力后助手被替换，旧助手也会拒绝请求而不降级执行。
    const value = await this._invoke({ operation: visible ? 'host_terminal_visible' : 'host_terminal', shell, script, cwd, timeoutMs,
      ...(visible ? { visible: true, keepOpenMs: holdMs } : {}) }, combined, timeoutMs + 10000, onOutput);
    if (!validEnvelope(value)) throw toolFailure('本机终端回执无效。', 'HOST_TERMINAL_INVALID_RESULT', 502);
    if (value.completed === true) {
      if (value.shell !== shell || value.cwd !== cwd || !Number.isInteger(value.exitCode) ||
          typeof value.stdout !== 'string' || typeof value.stderr !== 'string' || value.activeProcessesAfterExit !== 0 || value.error)
        return { value: { ...value, nativeReportedCompleted: true, completed: false, outcome: 'unknown',
          message: '本机终端完成回执未通过身份核验，请检查实际结果，不要自动重做。' },
          isError: true, status: 'unknown', code: 'HOST_TERMINAL_INVALID_RESULT' };
      if ((visible && !verifiesVisibleConsole(value, holdMs)) || (!visible && value.visibleRequested === true))
        return { value: { ...value, nativeReportedCompleted: true, completed: false, outcome: 'unknown',
          message: '本机终端窗口回执未通过核验，请检查实际结果，不要自动重做。' },
          isError: true, status: 'unknown', code: 'HOST_TERMINAL_INVALID_RESULT' };
      return { value, isError: value.exitCode !== 0 };
    }
    if (value.outcome === 'unknown') return { value, isError: true, status: 'unknown',
      code: value.cancelled ? 'TOOL_CANCELLED' : value.timedOut ? 'TOOL_TIMED_OUT' : value.outputLimitExceeded ? 'HOST_TERMINAL_OUTPUT_LIMIT' : 'HOST_TERMINAL_OUTCOME_UNKNOWN' };
    throw toolFailure(value.error?.message ?? '本机终端未返回执行回执。',
      /^HOST_TERMINAL_[A-Z0-9_]+$/.test(value.error?.code ?? '') ? value.error.code : 'HOST_TERMINAL_INVALID_RESULT', 502);
  }

  async close() { this.shutdown.abort(); await Promise.allSettled([...this.active]); }
}

function invokeHostTerminal(host, request, signal, timeoutMs, onOutput) {
  signal?.throwIfAborted();
  return new Promise((resolvePromise, reject) => {
    const child = spawn(host, [], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true, shell: false });
    const stdoutDecoder = new StringDecoder('utf8');
    let stdoutBuffer = '', stderrBytes = 0, startedReceipt, value, stopError, hardStop, settled = false, dispatched = false, sequence = 0;
    const partial = { stdout: '', stderr: '', consoleText: '' };
    const finish = (value, error) => {
      if (settled) return;
      settled = true; clearTimeout(watchdog); clearTimeout(hardStop);
      signal?.removeEventListener('abort', cancel);
      if (error) reject(error); else resolvePromise(value);
    };
    const stop = error => {
      if (stopError || settled) return;
      stopError = error;
      if (!child.stdin.destroyed) child.stdin.end('cancel\n');
      // Native execution has its own deadline; allow the job tree to terminate and report.
      // 原生执行有自己的截止时间，允许作业进程树完成终止并报告回执。
      hardStop = setTimeout(() => child.kill(), 4000);
    };
    const cancel = () => stop(Object.assign(new Error('本机终端已取消。'), { name: 'AbortError' }));
    const receiveLine = line => {
      let item;
      try { item = JSON.parse(line); } catch { return; }
      if (!validEnvelope(item)) return;
      if (item.event === 'host_terminal_started' && Number.isInteger(item.processId) && item.processId > 0) startedReceipt = item;
      else if (item.event === 'host_terminal_output') {
        const replace = item.stream === 'console' && item.replace === true;
        const text = replace ? item.text : item.delta;
        if (!startedReceipt || !Number.isSafeInteger(item.sequence) || item.sequence <= sequence ||
            !['stdout', 'stderr', 'console'].includes(item.stream) || (item.stream === 'console' && !replace) ||
            typeof text !== 'string' || text.length > 65536)
          return stop(toolFailure('本机终端输出事件无效。', 'HOST_TERMINAL_INVALID_RESULT', 502));
        sequence = item.sequence;
        if (replace) partial.consoleText = text;
        else partial[item.stream] = (partial[item.stream] + text).slice(-262144);
        try { onOutput?.({ sequence, stream: item.stream, text, replace }); }
        catch { stop(toolFailure('本机终端显示通道已关闭。', 'HOST_TERMINAL_DISPLAY_CLOSED', 502)); }
      } else value = item;
    };
    const drainLines = () => {
      for (let end; (end = stdoutBuffer.indexOf('\n')) >= 0;) {
        const line = stdoutBuffer.slice(0, end); stdoutBuffer = stdoutBuffer.slice(end + 1);
        if (Buffer.byteLength(line) > maxTransportBytes)
          return stop(toolFailure('本机终端回执过大。', 'HOST_TERMINAL_RESULT_TOO_LARGE', 413));
        receiveLine(line);
      }
      if (Buffer.byteLength(stdoutBuffer) > maxTransportBytes) {
        stdoutBuffer = '';
        stop(toolFailure('本机终端回执过大。', 'HOST_TERMINAL_RESULT_TOO_LARGE', 413));
      }
    };
    const watchdog = setTimeout(() => stop(toolFailure('本机终端超时。', 'HOST_TERMINAL_TIMED_OUT', 504)), timeoutMs);
    child.stdin.on('error', () => {});
    child.once('error', error => finish(null, error));
    child.stdout.on('data', chunk => {
      stdoutBuffer += stdoutDecoder.write(chunk);
      drainLines();
    });
    child.stderr.on('data', chunk => { stderrBytes += chunk.length;
      if (stderrBytes > 65536) stop(toolFailure('本机终端协议错误。', 'HOST_TERMINAL_INVALID_RESULT', 502)); });
    child.once('close', () => {
      stdoutBuffer += stdoutDecoder.end();
      drainLines();
      if (stdoutBuffer.trim() && Buffer.byteLength(stdoutBuffer) <= maxTransportBytes) receiveLine(stdoutBuffer.trim());
      if (request.operation === 'host_terminal_capabilities' && !stopError && validEnvelope(value) && typeof value.available === 'boolean') return finish(value);
      if (validEnvelope(value) && (value.completed === true || value.outcome === 'unknown')) return finish(value);
      if (validEnvelope(value) && value.outcome === 'not_started') return finish(value);
      // A helper can die after dispatch but before its started event. Missing acknowledgement is not proof of no effects.
      // 助手可能在收到请求后、发出 started 事件前退出；缺少确认不能证明没有副作用。
      if (startedReceipt || dispatched) return finish({ ...value, protocolVersion: 1, boundary, ...partial,
        completed: false, outcome: 'unknown', processId: startedReceipt?.processId,
        cancelled: stopError?.name === 'AbortError', timedOut: stopError?.code === 'HOST_TERMINAL_TIMED_OUT',
        message: '命令已发送，但没有完整完成回执，执行结果未知；请核验结果，不要自动重做。' });
      if (stopError) return finish(null, stopError);
      if (validEnvelope(value)) return finish(value);
      finish(null, toolFailure('本机终端助手未返回有效结果。', 'HOST_TERMINAL_INVALID_RESULT', 502));
    });
    signal?.addEventListener('abort', cancel, { once: true });
    if (signal?.aborted) return cancel();
    child.stdin.write(JSON.stringify(request) + '\n');
    dispatched = ['host_terminal', 'host_terminal_visible'].includes(request.operation);
  });
}
