import { spawn } from 'node:child_process';
import { access, lstat, realpath, rm } from 'node:fs/promises';
import { constants } from 'node:fs';
import { dirname, join, resolve, relative, isAbsolute } from 'node:path';
import { fileURLToPath } from 'node:url';
import { tmpdir } from 'node:os';
import { StringDecoder } from 'node:string_decoder';
import { supportsSkillExecution, verifiesSkillExecution } from './sandbox-skill.mjs';

const gatewayDirectory = dirname(fileURLToPath(import.meta.url));
const maximumHostOutputBytes = 2 * 1024 * 1024;

function failure(code, message) {
  return Object.assign(new Error(message), { code });
}

function abortError(result) {
  return Object.assign(new Error('Sandbox execution was cancelled.'), { name: 'AbortError', code: 'ABORT_ERR', sandboxResult: result });
}

/** Native helper only. A missing or failing AppContainer never runs a normal host process instead. */
export class SandboxRunner {
  #toolHostPath;
  #excludedRoots;
  #verifiedAppContainer = false;
  #verifiedSkillExecution = false;
  #stagingDirectories = new Set();
  #onStarted;

  constructor({ toolHostPath, excludedRoots = [], onStarted } = {}) {
    this.#toolHostPath = toolHostPath ? resolve(toolHostPath) : null;
    this.#excludedRoots = [...new Set([...excludedRoots,
      ...(process.env.KYNXA_DATA_HOME ? [process.env.KYNXA_DATA_HOME] : [])].map(path => resolve(path)))];
    this.#onStarted = onStarted;
  }

  get verifiedAppContainer() { return this.#verifiedAppContainer; }

  async #findToolHost() {
    if (process.platform !== 'win32') throw failure('SANDBOX_UNAVAILABLE', 'The AppContainer tool host requires Windows.');
    const candidates = this.#toolHostPath ? [this.#toolHostPath] : [
      join(gatewayDirectory, '..', 'ToolHost', 'KYNXA.ToolHost.exe'),
      join(gatewayDirectory, '..', 'tool-host', 'KYNXA.ToolHost.exe'),
      join(gatewayDirectory, '..', 'tool-host', 'bin', 'Debug', 'net10.0-windows', 'KYNXA.ToolHost.exe'),
      join(gatewayDirectory, '..', 'tool-host', 'bin', 'Release', 'net10.0-windows', 'KYNXA.ToolHost.exe')
    ];
    for (const path of candidates) {
      try {
        const info = await lstat(path);
        if (!info.isFile() || info.isSymbolicLink()) continue;
        await access(path, constants.R_OK);
        return path;
      } catch (error) { if (!['ENOENT', 'ENOTDIR'].includes(error.code)) throw error; }
    }
    throw failure('SANDBOX_UNAVAILABLE', 'The native KYNXA.ToolHost executable has not been built or packaged.');
  }

  async capabilities() {
    this.#verifiedAppContainer = false;
    this.#verifiedSkillExecution = false;
    try {
      const host = await this.#findToolHost();
      const status = await this.#invoke(host, { operation: 'capabilities' }, null, 10000);
      this.#verifiedAppContainer = status.protocolVersion === 1 && status.available === true &&
        status.sandbox === 'appcontainer' && status.failClosed === true && status.checksChildToken === true &&
        status.network === false && status.workspaceCopy === true;
      if (!this.#verifiedAppContainer) throw failure('SANDBOX_UNAVAILABLE', 'The native helper does not support the required fail-closed AppContainer protocol.');
      this.#verifiedSkillExecution = supportsSkillExecution(status);
      return status;
    } catch (error) {
      return { protocolVersion: 1, available: false, sandbox: 'appcontainer', commands: [], network: false,
        workspaceCopy: true, failClosed: true, checksChildToken: true, reason: error.message };
    }
  }

  async run({ workspaceRoot, command, args, timeoutMs = 30000, trustedManagedWorkspace = false, skill }, signal) {
    if (signal?.aborted) throw abortError();
    if (!['node', 'node.exe', 'cmd', 'cmd.exe'].includes(command))
      throw failure('SANDBOX_COMMAND_UNSUPPORTED', 'Only node and cmd are currently supported in the isolated terminal.');
    if (typeof workspaceRoot !== 'string' || !isAbsolute(workspaceRoot) || workspaceRoot.startsWith('\\\\'))
      throw failure('SANDBOX_INVALID_WORKSPACE', 'An absolute local workspace directory is required.');
    if (!Array.isArray(args) || args.length > 128 || args.some(argument => typeof argument !== 'string' || argument.includes('\0')) ||
        args.reduce((length, argument) => length + argument.length, 0) > 24000)
      throw failure('SANDBOX_INVALID_REQUEST', 'Terminal arguments must be a bounded array of strings.');
    if (args.some(argument => argument.startsWith('--inspect') || argument.startsWith('--debug')))
      throw failure('SANDBOX_COMMAND_UNSUPPORTED', 'Debugger ports are not enabled in the sandbox.');
    if (['cmd', 'cmd.exe'].includes(command) && (args.length !== 3 || args[0].toLowerCase() !== '/d' || args[1].toLowerCase() !== '/c' || !args[2].trim()))
      throw failure('SANDBOX_INVALID_REQUEST', "cmd requires ['/d', '/c', 'command text']; interactive input and AutoRun are disabled.");
    if (!Number.isInteger(timeoutMs) || timeoutMs < 100 || timeoutMs > 120000)
      throw failure('SANDBOX_INVALID_REQUEST', 'Timeout must be between 100 and 120000 milliseconds.');
    const host = await this.#findToolHost();
    let result;
    try {
      result = await this.#invoke(host, { operation: 'run', workspaceRoot: resolve(workspaceRoot), command,
        args, timeoutMs, nodeExecutable: process.execPath, excludedRoots: this.#excludedRoots,
        trustedManagedWorkspace: trustedManagedWorkspace === true, ...(skill ? { skill } : {}) }, signal, timeoutMs + 30000);
    } catch (error) {
      const completed = error.sandboxResult;
      // A cancellation can arrive between native completion and pipe close. Its retained copy must still be owned and cleaned.
      if (error.name === 'AbortError' && completed?.protocolVersion === 1 && completed.tokenVerified === true &&
          completed.activeProcessesAfterExit === 0 && completed.cancelled === false && typeof completed.stagingDirectory === 'string') {
        this.#stagingDirectories.add(completed.stagingDirectory);
        try { await this.cleanup(completed.stagingDirectory); }
        catch (cleanupError) { error.cleanupError = cleanupError.code ?? 'SANDBOX_CLEANUP_FAILED'; }
      }
      throw error;
    }
    if (result.protocolVersion !== 1 || result.sandbox !== 'appcontainer' || result.tokenVerified !== true ||
        result.workspaceCopy !== true || result.activeProcessesAfterExit !== 0)
      throw failure('SANDBOX_START_FAILED', 'The native helper did not verify isolated execution and process cleanup.');
    if (!result.cancelled) this.#stagingDirectories.add(result.stagingDirectory);
    return result;
  }

  async runSkill({ package: prepared, workspaceRoot, args = [], timeoutMs, trustedManagedWorkspace }, signal) {
    if (!prepared || !Array.isArray(prepared.files) || prepared.files.length > 128 ||
        typeof prepared.skillRoot !== 'string' || typeof prepared.scriptRelativePath !== 'string')
      throw failure('SANDBOX_INVALID_SKILL', 'A verified skill package manifest is required.');
    signal?.throwIfAborted();
    await this.capabilities();
    if (!this.#verifiedSkillExecution)
      throw failure('SANDBOX_SKILL_UNSUPPORTED', 'The native helper does not advertise hash-checked read-only skill packages.');
    const result = await this.run({ workspaceRoot, command: 'node', args, timeoutMs, trustedManagedWorkspace,
      skill: { root: prepared.skillRoot, script: prepared.scriptRelativePath,
        files: prepared.files.map(file => ({ path: file.relativePath, size: file.size, sha256: file.sha256 })) } }, signal);
    if (!verifiesSkillExecution(result, prepared)) {
      if (typeof result.stagingDirectory === 'string' && this.#stagingDirectories.has(result.stagingDirectory))
        await this.cleanup(result.stagingDirectory);
      throw failure('SANDBOX_INVALID_RESULT', 'The native helper did not prove execution of the selected skill package.');
    }
    return result;
  }

  /** Only directories returned by this runner may be removed; junctions below are not followed by fs.rm. */
  async cleanup(stagingDirectory) {
    if (!this.#stagingDirectories.has(stagingDirectory)) throw failure('SANDBOX_INVALID_CLEANUP', 'The staging directory does not belong to this runner.');
    const root = await realpath(join(tmpdir(), 'kynxa-tool-sandbox'));
    const run = dirname(resolve(stagingDirectory));
    const runRelative = relative(root, run);
    if (!runRelative || runRelative.startsWith('..') || isAbsolute(runRelative) || runRelative.includes('\\') || runRelative.includes('/'))
      throw failure('SANDBOX_INVALID_CLEANUP', 'Refusing cleanup outside an owned sandbox run.');
    const info = await lstat(run);
    if (info.isSymbolicLink() || !info.isDirectory()) throw failure('SANDBOX_INVALID_CLEANUP', 'The run directory was replaced by a link.');
    await rm(run, { recursive: true, force: true });
    this.#stagingDirectories.delete(stagingDirectory);
  }

  async cleanupAll() {
    const outcomes = await Promise.allSettled([...this.#stagingDirectories].map(stage => this.cleanup(stage)));
    const failures = outcomes.filter(outcome => outcome.status === 'rejected');
    if (failures.length) throw failure('SANDBOX_CLEANUP_FAILED', `${failures.length} retained sandbox workspace(s) could not be removed.`);
  }

  async #invoke(host, request, signal, maximumWaitMs) {
    if (signal?.aborted) throw abortError();
    return new Promise((resolveResult, reject) => {
      const child = spawn(host, [], { shell: false, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
      const decoder = new StringDecoder('utf8');
      let stdoutBytes = 0, pending = '', result, aborted = false, hardStopTimer, settled = false;
      const finish = (error, result) => {
        if (settled) return;
        settled = true;
        clearTimeout(watchdog);
        clearTimeout(hardStopTimer);
        signal?.removeEventListener('abort', cancel);
        if (error) reject(error); else resolveResult(result);
      };
      const cancel = () => {
        aborted = true;
        if (!child.stdin.destroyed) child.stdin.end('cancel\n');
        // Graceful cancellation releases the unique profile; closing the job handle is the fail-closed fallback.
        hardStopTimer ??= setTimeout(() => child.kill(), 5000);
      };
      const watchdog = setTimeout(() => {
        child.kill();
        finish(failure('SANDBOX_HOST_TIMEOUT', 'The native sandbox helper exceeded its startup or cleanup deadline.'));
      }, maximumWaitMs);
      signal?.addEventListener('abort', cancel, { once: true });
      child.stdin.on('error', () => { /* The close/error event below supplies the authoritative outcome. */ });
      child.stdout.on('data', chunk => {
        stdoutBytes += chunk.length;
        if (stdoutBytes > maximumHostOutputBytes) {
          child.kill();
          finish(failure('SANDBOX_HOST_OUTPUT_LIMIT', 'The native helper returned an oversized response.'));
        } else {
          pending += decoder.write(chunk);
          let newline;
          while ((newline = pending.indexOf('\n')) !== -1) {
            const line = pending.slice(0, newline).trim();
            pending = pending.slice(newline + 1);
            if (!line) continue;
            try {
              const record = JSON.parse(line);
              if (record.event === 'sandbox_started' && record.protocolVersion === 1 && request.operation === 'run') {
                this.#onStarted?.(record);
              } else if (record.event || result !== undefined) {
                throw failure('SANDBOX_START_FAILED', 'The native helper returned an invalid or repeated record.');
              } else result = record;
            } catch (error) {
              child.kill();
              finish(failure('SANDBOX_START_FAILED', error.message));
            }
          }
        }
      });
      child.stderr.on('data', () => { /* Runtime diagnostics are not copied into tool output; JSON errors are authoritative. */ });
      child.on('error', error => finish(failure('SANDBOX_UNAVAILABLE', error.message)));
      child.on('close', code => {
        try {
          pending += decoder.end();
          if (pending.trim() || result === undefined) throw new Error('Missing final response.');
        }
        catch { finish(aborted ? abortError() : failure('SANDBOX_START_FAILED', `The native helper exited without a valid response (${code}).`)); return; }
        if (aborted || result.cancelled) { finish(abortError(result)); return; }
        if (result.error) { finish(failure(result.error.code ?? 'SANDBOX_START_FAILED', result.error.message)); return; }
        if (code !== 0) { finish(failure('SANDBOX_START_FAILED', `The native helper failed (${code}).`)); return; }
        finish(null, result);
      });
      child.stdin.write(JSON.stringify(request) + '\n');
      if (request.operation === 'capabilities') child.stdin.end();
      if (signal?.aborted) cancel();
    });
  }
}
