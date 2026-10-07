import { spawn } from 'node:child_process';
import { basename, isAbsolute } from 'node:path';
import { StringDecoder } from 'node:string_decoder';
import { findNativeToolHost } from './tool-host-path.mjs';
import { bindLocalPath, revalidateLocalPathBinding, toolFailure } from '../platform/tool-paths.mjs';
import { computerKeyNames } from '../official-tools/Tools/computer.mjs';

const actions = new Set(['windows', 'apps', 'screenshot', 'read', 'launch', 'window', 'activate', 'move', 'click', 'scroll', 'drag', 'type', 'key']);
const shells = /^(?:cmd|powershell|pwsh|wscript|cscript|mshta|rundll32|regsvr32|node|python(?:\d+(?:\.\d+)*)?|py|bash|sh|wsl|wt)\.exe$/i;
const maxStdoutBytes = 8 * 1024 * 1024;
const definiteLaunchFailures = new Set(['DESKTOP_LAUNCH_BLOCKED', 'DESKTOP_LAUNCH_FAILED', 'DESKTOP_INVALID_REQUEST',
  'DESKTOP_UNAVAILABLE', 'DESKTOP_BUSY', 'DESKTOP_CANCELLED', 'DESKTOP_ACCESS_DENIED']);

/**
 * Desktop operations have their own protocol and host boundary; they never run through the terminal sandbox.
 * 桌面操作有独立协议和宿主边界，不经过终端沙箱执行。
 */
export class DesktopRunner {
  constructor({ toolHostPath, invoke } = {}) {
    this.toolHostPath = toolHostPath;
    this.invoke = invoke ?? invokeDesktopHost;
    this.shutdown = new AbortController();
    this.active = new Set();
  }

  async _invoke(request, signal, timeoutMs) {
    const host = await findNativeToolHost(this.toolHostPath, 'DESKTOP_UNAVAILABLE');
    this.shutdown.signal.throwIfAborted(); signal?.throwIfAborted();
    const operation = this.invoke(host, request, signal, timeoutMs);
    this.active.add(operation);
    try { return await operation; }
    finally { this.active.delete(operation); }
  }

  async capabilities() {
    try {
      const value = await this._invoke({ operation: 'desktop_capabilities' }, this.shutdown.signal, 10000);
      if (value.protocolVersion !== 1 || value.boundary !== 'host-desktop' || value.available !== true ||
          value.interactiveWindows !== true || !Array.isArray(value.operations) || value.operations.some(action => !actions.has(action)))
        throw toolFailure('当前原生助手没有可用的本机桌面控制能力。', 'DESKTOP_UNAVAILABLE', 503);
      return value;
    } catch (error) {
      return { protocolVersion: 1, boundary: 'host-desktop', available: false, interactiveWindows: false,
        operations: [], reason: error.message };
    }
  }

  async run(action, arguments_, signal) {
    signal?.throwIfAborted(); this.shutdown.signal.throwIfAborted();
    if (!actions.has(action)) throw toolFailure('不支持此本机操作。', 'DESKTOP_ACTION_UNSUPPORTED');
    const isBoundedInteger = (value, minimum, maximum) => Number.isSafeInteger(value) && value >= minimum && value <= maximum;
    for (const name of ['crop', 'region']) if (arguments_[name] !== undefined) {
      const area = arguments_[name];
      if (!area || typeof area !== 'object' || Array.isArray(area) || Object.keys(area).some(key => !['x', 'y', 'width', 'height'].includes(key)) ||
          !isBoundedInteger(area.x, 0, 32767) || !isBoundedInteger(area.y, 0, 32767) || !isBoundedInteger(area.width, 1, 8192) || !isBoundedInteger(area.height, 1, 8192))
        throw toolFailure('截图或读取区域需要有效的客户区像素坐标和尺寸。', 'DESKTOP_INVALID_COORDINATES');
    }
    if (action === 'read' && (arguments_.timeoutMs !== undefined && !isBoundedInteger(arguments_.timeoutMs, 500, 5000) ||
        arguments_.elementId !== undefined && (typeof arguments_.elementId !== 'string' || arguments_.elementId.length > 256 || !/^-?\d+(?:,-?\d+){0,31}$/.test(arguments_.elementId))))
      throw toolFailure('读取期限或元素标识无效。', 'DESKTOP_INVALID_REQUEST');
    if (action === 'window' && (!['resize', 'maximize', 'minimize', 'restore'].includes(arguments_.mode) ||
        arguments_.mode === 'resize' && (!isBoundedInteger(arguments_.width, 64, 8192) || !isBoundedInteger(arguments_.height, 64, 8192))))
      throw toolFailure('窗口调整需要有效模式；调整大小时需指定客户区宽高。', 'DESKTOP_INVALID_REQUEST');
    if (action === 'key' && !computerKeyNames.includes(arguments_.key))
      throw toolFailure('不支持此按键组合。', 'DESKTOP_INVALID_KEY');
    if (action === 'launch' && arguments_.background !== undefined && typeof arguments_.background !== 'boolean')
      throw toolFailure('后台启动选项必须为布尔值。', 'DESKTOP_INVALID_REQUEST');
    let launchBinding;
    if (action === 'launch') {
      const path = arguments_.appPath;
      if (typeof path !== 'string' || !isAbsolute(path) || /^\\\\/.test(path) || !/\.exe$/i.test(path) || shells.test(basename(path)))
        throw toolFailure('打开软件需要本地应用 .exe 路径；命令解释器请使用对应的终端工具。', 'DESKTOP_INVALID_APPLICATION');
      launchBinding = await bindLocalPath(path, { allowHardLinks: true });
      const info = await revalidateLocalPathBinding(launchBinding);
      if (!info.isFile()) throw toolFailure('应用路径不是本地程序文件。', 'DESKTOP_INVALID_APPLICATION');
    } else if (!['windows', 'apps'].includes(action)) {
      if (typeof arguments_.windowId !== 'string' || !/^[1-9]\d{0,19}$/.test(arguments_.windowId) ||
          !Number.isSafeInteger(arguments_.processId) || arguments_.processId <= 0)
        throw toolFailure('请使用 computer.windows 返回的窗口与进程身份。', 'DESKTOP_INVALID_TARGET');
    }
    const { reason, ...parameters } = arguments_;
    if (launchBinding) parameters.appPath = launchBinding.path;
    const combined = signal ? AbortSignal.any([signal, this.shutdown.signal]) : this.shutdown.signal;
    let value;
    try { value = await this._invoke({ operation: 'desktop', action, ...parameters }, combined,
      action === 'read' ? (parameters.timeoutMs ?? 3000) + 1500 : 20000); }
    catch (error) {
      if (action !== 'launch' || error.desktopOutcomeUnknown !== true) throw error;
      return { value: { action, boundary: 'host-desktop', completed: false, outcome: 'unknown',
        message: '启动请求没有可核验的完成回执；软件可能已打开，请先检查，不要自动重复启动。' },
        isError: true, status: 'unknown', code: error.name === 'AbortError' ? 'TOOL_CANCELLED' : error.code ?? 'DESKTOP_OUTCOME_UNKNOWN' };
    }
    if (value?.error?.partial === true && Number.isInteger(value.error.deliveredInputEvents) && value.error.deliveredInputEvents > 0)
      return { value: { action, boundary: 'host-desktop', completed: false, partial: true,
        deliveredInputEvents: value.error.deliveredInputEvents, message: value.error.message }, isError: true, status: 'unknown',
        code: combined.aborted ? 'TOOL_CANCELLED' : 'DESKTOP_PARTIAL_INPUT' };
    const matchesRegion = (expected, actual) => actual && ['x', 'y', 'width', 'height'].every(key => actual[key] === expected[key]);
    const extensionMismatch = action === 'launch' && parameters.background !== undefined &&
      (value?.backgroundRequested !== parameters.background || value?.backgroundMode !== (parameters.background ? 'best-effort-no-activate' : 'normal')) ||
      action === 'window' && value?.mode !== parameters.mode ||
      action === 'screenshot' && parameters.crop && (!matchesRegion(parameters.crop, value?.crop) ||
        value?.width !== parameters.crop.width || value?.height !== parameters.crop.height) ||
      action === 'read' && (parameters.region && !matchesRegion(parameters.region, value?.region) ||
        parameters.elementId !== undefined && value?.elementId !== parameters.elementId ||
        parameters.timeoutMs !== undefined && value?.timeoutMs !== parameters.timeoutMs);
    if (value?.protocolVersion !== 1 || value.boundary !== 'host-desktop' || value.action !== action || value.completed !== true || extensionMismatch) {
      if (action === 'launch') return { value: { action, boundary: 'host-desktop', completed: false, outcome: 'unknown',
        nativeReportedCompleted: value?.completed === true,
        message: '启动回执未通过身份核验；软件可能已打开，请先检查，不要自动重复启动。' },
        isError: true, status: 'unknown', code: 'DESKTOP_INVALID_RESULT' };
      throw toolFailure('原生本机操作缺少完成回执。', 'DESKTOP_INVALID_RESULT', 502);
    }
    if (action === 'screenshot') {
      if (value.mimeType !== 'image/png' || typeof value.data !== 'string' || !/^[A-Za-z0-9+/]+={0,2}$/.test(value.data) ||
          Buffer.byteLength(value.data, 'base64') > 4 * 1024 * 1024 ||
          !Number.isInteger(value.width) || !Number.isInteger(value.height) || value.width <= 0 || value.height <= 0 || value.width * value.height > 16000000)
        throw toolFailure('本机截图格式或尺寸不受支持。', 'DESKTOP_INVALID_RESULT', 502);
      const png = Buffer.from(value.data, 'base64');
      if (png.toString('base64') !== value.data || !png.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10])))
        throw toolFailure('本机截图不是有效的 PNG 编码。', 'DESKTOP_INVALID_RESULT', 502);
      const { data, ...metadata } = value;
      const canonical = { content: [{ type: 'image', mimeType: 'image/png', data }], structuredContent: metadata, isError: false };
      return { canonical, content: JSON.stringify({ ...metadata, image: 'PNG saved; inspect in the local tool result viewer. No image pixels were sent to the text model.' }), isError: false };
    }
    return { value, isError: false };
  }

  async close() { this.shutdown.abort(); await Promise.allSettled([...this.active]); }
}

// The optional process factory is an isolated transport-test seam, never a model/tool parameter.
// 可选进程工厂只用于隔离传输测试，不属于模型或工具参数。
export function invokeDesktopHost(host, request, signal, timeoutMs, spawnProcess = spawn) {
  signal?.throwIfAborted();
  return new Promise((resolve, reject) => {
    const child = spawnProcess(host, [], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true, shell: false });
    const stdoutDecoder = new StringDecoder('utf8');
    let stdoutBuffer = '', stdoutBytes = 0, stderrBytes = 0, stopError, settled = false, hardStop, drainTimer;
    let replyFrame, frameCount = 0, exited = false, spawned = false, submitted = false, invalidTransport;
    const uncertain = error => {
      if (spawned && submitted && request.operation === 'desktop') error.desktopOutcomeUnknown = true;
      return error;
    };
    const finish = (value, error) => {
      if (settled) return;
      settled = true; clearTimeout(watchdog); clearTimeout(hardStop); clearTimeout(drainTimer);
      signal?.removeEventListener('abort', cancel);
      // Dispose only this RPC's IPC. Launched apps have independent lifetimes and must not be killed here.
      // 仅释放本次 RPC 的通信资源；已启动软件生命周期独立，不能在此终止。
      child.stdin.destroy(); child.stdout.destroy(); child.stderr.destroy();
      if (!exited && child.pid && !child.killed) child.kill();
      if (error) reject(error); else resolve(value);
    };
    const stop = error => {
      if (settled || stopError) return;
      stopError = error;
      if (!child.stdin.destroyed) child.stdin.end('cancel\n');
      // Killing the helper need not close pipes accidentally inherited by a GUI app.
      // Settlement has its own deadline and never waits indefinitely for a close event.
      // 结束助手进程未必关闭 GUI 软件意外继承的管道；结算有独立期限，不无限等待 close 事件。
      hardStop = setTimeout(() => { if (!exited) child.kill(); settleReply(); }, 1000);
    };
    const cancel = () => stop(Object.assign(new Error('本机操作已取消。'), { name: 'AbortError' }));
    const watchdog = setTimeout(() => stop(toolFailure('本机操作超时。', 'DESKTOP_TIMED_OUT', 504)), timeoutMs);
    const parseFrame = line => {
      if (!line.trim() || settled) return;
      try {
        const value = JSON.parse(line);
        if (++frameCount !== 1) throw new Error('Multiple native replies');
        replyFrame = value;
      } catch {
        invalidTransport ??= toolFailure('本机助手未返回有效结果。', 'DESKTOP_INVALID_RESULT', 502);
        stop(invalidTransport);
      }
    };
    const flushFrames = () => {
      for (let index; (index = stdoutBuffer.indexOf('\n')) >= 0;) {
        const line = stdoutBuffer.slice(0, index); stdoutBuffer = stdoutBuffer.slice(index + 1);
        parseFrame(line);
      }
    };
    const settleReply = () => {
      if (settled) return;
      if (stdoutBuffer.trim() && !invalidTransport) parseFrame(stdoutBuffer);
      stdoutBuffer = '';
      const value = replyFrame;
      // Discovery is a capability response, not the completion receipt of an executed action.
      // 能力发现返回的是能力说明，不能当作已执行动作的完成回执。
      if (!stopError && request.operation === 'desktop_capabilities' && value?.protocolVersion === 1 &&
          value.boundary === 'host-desktop' && typeof value.available === 'boolean' && !value.error)
        return finish(value);
      // A complete receipt remains proof even when a cancellation/deadline races with helper exit.
      // 完整回执仍可证明完成，即使取消或超时与助手退出同时发生。
      if (!invalidTransport && (!stopError || stopError.name === 'AbortError' || stopError.code === 'DESKTOP_TIMED_OUT') &&
          value?.protocolVersion === 1 && value.boundary === 'host-desktop' && value.completed === true && !value.error)
        return finish(value);
      if (value?.error?.partial === true && Number.isInteger(value.error.deliveredInputEvents) && value.error.deliveredInputEvents > 0)
        return finish(value);
      if (invalidTransport) return finish(null, uncertain(invalidTransport));
      if (stopError) return finish(null, uncertain(stopError));
      const error = value?.error;
      const failure = toolFailure(typeof error?.message === 'string' ? error.message : '本机助手未返回有效结果。',
        typeof error?.code === 'string' && /^DESKTOP_[A-Z0-9_]+$/.test(error.code) ? error.code : 'DESKTOP_INVALID_RESULT', 502);
      // Only launch validation/start failures prove that no GUI process was created.
      // A generic native failure/timeout can happen after an effect and must not invite a repeated launch.
      // 只有启动校验或启动失败能证明 GUI 进程未创建；一般原生错误或超时可能发生在副作用之后，不能诱发自动重复启动。
      const unconfirmed = !error || error.code === 'DESKTOP_TIMEOUT' ||
        (request.action === 'launch' && !definiteLaunchFailures.has(error.code));
      finish(null, unconfirmed ? uncertain(failure) : failure);
    };
    child.stdin.on('error', () => {});
    child.once('error', error => finish(null, error));
    child.once('spawn', () => { spawned = true; });
    child.stdout.on('data', chunk => {
      if (settled) return;
      stdoutBytes += chunk.length;
      if (stdoutBytes > maxStdoutBytes) {
        invalidTransport = toolFailure('本机结果超过保存上限。', 'DESKTOP_RESULT_TOO_LARGE', 413);
        return stop(invalidTransport);
      }
      stdoutBuffer += stdoutDecoder.write(chunk); flushFrames();
      if (exited && replyFrame) settleReply();
    });
    child.stderr.on('data', chunk => {
      if (settled) return;
      stderrBytes += chunk.length;
      if (stderrBytes > 65536) {
        invalidTransport = toolFailure('本机助手错误输出过大。', 'DESKTOP_INVALID_RESULT', 502);
        stop(invalidTransport);
      }
    });
    child.stdout.on('error', () => { if (!settled) stop(toolFailure('本机助手结果连接中断。', 'DESKTOP_INVALID_RESULT', 502)); });
    child.stderr.on('error', () => { if (!settled) stop(toolFailure('本机助手错误连接中断。', 'DESKTOP_INVALID_RESULT', 502)); });
    child.stdout.once('end', () => {
      if (settled) return;
      stdoutBuffer += stdoutDecoder.end(); flushFrames();
      if (exited) settleReply();
    });
    child.once('exit', () => {
      exited = true;
      if (replyFrame) settleReply();
      else drainTimer = setTimeout(settleReply, 100);
    });
    child.once('close', settleReply);
    signal?.addEventListener('abort', cancel, { once: true });
    if (signal?.aborted) return cancel();
    child.stdin.write(JSON.stringify(request) + '\n'); submitted = true;
  });
}
