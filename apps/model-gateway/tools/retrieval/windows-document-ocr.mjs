import { spawn } from 'node:child_process';
import { findNativeToolHost } from '../tool-host-path.mjs';
import { runResourceTask } from '../../platform/resources/resource-task.mjs';
import { documentExtractionFailure } from './document-extraction-contracts.mjs';

/** Recognize only caller-authorized PDF bytes through the packaged Windows helper, never a shell or user Python.
 * 只通过随包 Windows 助手识别调用方已授权的 PDF 字节，不使用 shell 或用户 Python。
 */
export async function recognizePdfPages(bytes, pages, { signal, resourceService, toolHostPath,
  spawnFactory = spawn, maximumOutputBytes = 2 * 1024 * 1024, timeoutMs = 8000 } = {}) {
  signal?.throwIfAborted();
  if (!(bytes instanceof Uint8Array) || !bytes.length || bytes.length > 32 * 1024 * 1024 ||
      !Array.isArray(pages) || !pages.length || pages.length > 100 ||
      pages.some(page => !Number.isSafeInteger(page) || page < 1 || page > 1000000) || new Set(pages).size !== pages.length ||
      !Number.isSafeInteger(maximumOutputBytes) || maximumOutputBytes < 1 || maximumOutputBytes > 2 * 1024 * 1024)
    throw documentExtractionFailure('DOCUMENT_INVALID_INPUT');
  const executable = await findNativeToolHost(toolHostPath, 'OCR_UNAVAILABLE');
  return runResourceTask(resourceService, { kind: 'background', cpuThreads: 1,
    memoryBytes: 384 * 1024 * 1024 + bytes.byteLength * 4 + maximumOutputBytes * 2, waitMs: timeoutMs }, async lease => {
    let child, failure, timer, killTimer, stdout = '', outputBytes = 0;
    const stop = error => {
      if (failure) return;
      failure = error;
      try { child?.stdin.write('{"operation":"cancel"}\n'); } catch { /* Close remains authoritative. / 退出观察仍为准。 */ }
      killTimer = setTimeout(() => { try { child?.kill(); } catch { /* Keep the lease until close. / 保留租约直至退出。 */ } }, 2000);
    };
    const abort = () => stop(Object.assign(new Error('OCR cancelled. / OCR 已取消。'), { name: 'AbortError', code: 'ABORT_ERR' }));
    const closed = new Promise((resolve, reject) => {
      try {
        child = spawnFactory(executable, ['--document-ocr'], { windowsHide: true, shell: false,
          stdio: ['pipe', 'pipe', 'ignore'], env: { SystemRoot: process.env.SystemRoot, WINDIR: process.env.WINDIR,
            PATH: process.env.PATH, DOTNET_EnableDiagnostics: '0' } });
      } catch { reject(documentExtractionFailure('OCR_UNAVAILABLE')); return; }
      child.once('error', () => stop(documentExtractionFailure('OCR_UNAVAILABLE')));
      child.stdin.on('error', () => stop(documentExtractionFailure('OCR_FAILED')));
      child.stdout.setEncoding('utf8');
      child.stdout.on('data', chunk => {
        outputBytes += Buffer.byteLength(chunk);
        if (outputBytes > maximumOutputBytes + 64 * 1024) stop(documentExtractionFailure('DOCUMENT_OUTPUT_LIMIT'));
        else stdout += chunk;
      });
      child.once('close', code => {
        if (failure) { reject(failure); return; }
        try {
          const result = JSON.parse(stdout);
          if (result.errorCode || result.code) throw documentExtractionFailure(/^OCR_[A-Z_]+$/u.test(result.errorCode ?? result.code)
            ? result.errorCode ?? result.code : 'OCR_FAILED');
          if (code !== 0 || result.protocolVersion !== 1 || result.schemaVersion !== 1 || result.boundary !== 'document-ocr' || result.completed !== true ||
              typeof result.version !== 'string' || !result.version.startsWith('windows-media-ocr-v1') || result.version.length > 128 ||
              typeof result.language !== 'string' || !result.language || result.language.length > 128 ||
              /[\x00-\x1f]/u.test(result.version + result.language) ||
              !Array.isArray(result.pages) || result.pages.length !== pages.length ||
              result.pages.some((item, index) => item.page !== pages[index] || typeof item.text !== 'string') ||
              result.pages.reduce((sum, item) => sum + Buffer.byteLength(item.text), 0) > maximumOutputBytes)
            throw documentExtractionFailure('OCR_INVALID_RESULT');
          resolve(result);
        } catch (error) { reject(error.code ? error : documentExtractionFailure('OCR_INVALID_RESULT')); }
      });
    });
    timer = setTimeout(() => stop(documentExtractionFailure('DOCUMENT_DECODER_TIMEOUT')), Math.min(timeoutMs, 8000));
    signal?.addEventListener('abort', abort, { once: true });
    try {
      if (lease && child) {
        const receipt = await resourceService.registerExecutor(lease.leaseId, { processId: child.pid });
        if (receipt?.status !== 'registered') stop(documentExtractionFailure('DOCUMENT_RESOURCE_FAILED'));
      }
      if (signal?.aborted) abort();
      // EOF cancels the helper, so keep stdin open while owned recognition settles.
      // EOF 会取消助手，因此识别真正结束前保持输入管道打开。
      if (!failure && child) child.stdin.write(JSON.stringify({ operation: 'document_ocr', schemaVersion: 1,
        bytesBase64: Buffer.from(bytes).toString('base64'), pages, maximumOutputBytes }) + '\n');
      return await closed;
    } catch (error) {
      stop(error);
      await closed.catch(() => {});
      throw error;
    } finally { clearTimeout(timer); clearTimeout(killTimer); signal?.removeEventListener('abort', abort); }
  }, { signal });
}
